use crate::prelude::*;
use rusqlite::{params, Connection};
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct FeedbackRecord {
    pub slug: String,
    pub verdict: String,
    pub tags: Vec<String>,
    pub game_version: String,
    pub loader: String,
    pub source: String,
    pub timestamp: String,
}

#[derive(Clone, serde::Serialize, serde::Deserialize)]
pub struct PackRecord {
    pub name: String,
    pub mod_slugs: Vec<String>,
    pub created_at: String,
}

pub struct UserDatabase {
    pub feedback: Vec<FeedbackRecord>,
    pub packs: Vec<PackRecord>,
    storage_path: String,
}

impl UserDatabase {
    pub fn load(path: &str) -> Self {
        let mut db = Self {
            feedback: Vec::new(),
            packs: Vec::new(),
            storage_path: path.to_string(),
        };
        if let Ok(conn) = open(&db.storage_path) {
            if db.read_from(&conn).is_ok() {
                return db;
            }
        }
        db
    }

    pub fn save(&self) -> Result<()> {
        let conn = open(&self.storage_path)?;
        self.save_inner(&conn)
    }

    pub fn rate(&mut self, record: FeedbackRecord) {
        self.feedback.push(record);
    }

    pub fn rated_slugs(&self) -> HashSet<String> {
        self.feedback.iter().map(|f| f.slug.clone()).collect()
    }

    pub fn tag_weights(&self) -> BTreeMap<String, f64> {
        let mut weights = BTreeMap::new();
        for feedback in &self.feedback {
            let delta = if feedback.verdict == "like" {
                1.0
            } else {
                -1.0
            };
            for tag in &feedback.tags {
                *weights.entry(tag.clone()).or_insert(0.0) += delta;
            }
        }
        weights
    }

    pub fn top_tags(&self, n: usize) -> Vec<String> {
        let mut pairs: Vec<_> = self
            .tag_weights()
            .into_iter()
            .filter(|(_, w)| *w > 0.0)
            .collect();
        pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        pairs.into_iter().take(n).map(|(tag, _)| tag).collect()
    }

    pub fn summary(&self) -> String {
        let likes = self.feedback.iter().filter(|f| f.verdict == "like").count();
        format!(
            "反馈 {} 条（喜欢 {} / 不喜欢 {}）| 组包 {} 次",
            self.feedback.len(),
            likes,
            self.feedback.len() - likes,
            self.packs.len()
        )
    }

    fn read_from(&mut self, conn: &Connection) -> Result<()> {
        let mut stmt = conn.prepare("SELECT id, slug, verdict, game_version, loader, source, timestamp FROM feedback ORDER BY id")?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                FeedbackRecord {
                    slug: row.get(1)?,
                    verdict: row.get(2)?,
                    tags: Vec::new(),
                    game_version: row.get(3)?,
                    loader: row.get(4)?,
                    source: row.get(5)?,
                    timestamp: row.get(6)?,
                },
            ))
        })?;
        let mut ids = Vec::new();
        self.feedback = rows
            .collect::<rusqlite::Result<Vec<_>>>()?
            .into_iter()
            .map(|(id, record)| {
                ids.push(id);
                record
            })
            .collect();
        let mut tags = conn.prepare("SELECT feedback_id, tag FROM feedback_tags")?;
        for row in tags.query_map([], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })? {
            let (id, tag) = row?;
            if let Some(index) = ids.iter().position(|saved| *saved == id) {
                self.feedback[index].tags.push(tag);
            }
        }
        let mut packs = conn.prepare("SELECT id, name, created_at FROM packs ORDER BY id")?;
        self.packs = packs
            .query_map([], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .map(|row| {
                let (id, name, created_at) = row?;
                let mut mods = conn.prepare(
                    "SELECT mod_slug FROM pack_mods WHERE pack_id = ?1 ORDER BY position",
                )?;
                let slugs = mods
                    .query_map([id], |r| r.get(0))?
                    .collect::<rusqlite::Result<Vec<String>>>()?;
                Ok(PackRecord {
                    name,
                    created_at,
                    mod_slugs: slugs,
                })
            })
            .collect::<Result<Vec<_>, rusqlite::Error>>()?;
        Ok(())
    }

    fn save_inner(&self, conn: &Connection) -> Result<()> {
        let tx = conn.unchecked_transaction()?;
        tx.execute_batch("DELETE FROM feedback_tags; DELETE FROM feedback; DELETE FROM pack_mods; DELETE FROM packs;")?;
        for feedback in &self.feedback {
            tx.execute("INSERT INTO feedback (slug, verdict, game_version, loader, source, timestamp) VALUES (?1, ?2, ?3, ?4, ?5, ?6)", params![feedback.slug, feedback.verdict, feedback.game_version, feedback.loader, feedback.source, feedback.timestamp])?;
            let id = tx.last_insert_rowid();
            for tag in &feedback.tags {
                tx.execute(
                    "INSERT INTO feedback_tags (feedback_id, tag) VALUES (?1, ?2)",
                    params![id, tag],
                )?;
            }
        }
        for pack in &self.packs {
            tx.execute(
                "INSERT INTO packs (name, created_at) VALUES (?1, ?2)",
                params![pack.name, pack.created_at],
            )?;
            let id = tx.last_insert_rowid();
            for (position, slug) in pack.mod_slugs.iter().enumerate() {
                tx.execute(
                    "INSERT INTO pack_mods (pack_id, position, mod_slug) VALUES (?1, ?2, ?3)",
                    params![id, position as i64, slug],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}

fn open(path: &str) -> Result<Connection> {
    if let Some(parent) = std::path::Path::new(path)
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(path).with_context(|| format!("无法打开 SQLite 数据库: {path}"))?;
    conn.execute_batch("PRAGMA foreign_keys = ON;
        CREATE TABLE IF NOT EXISTS feedback (id INTEGER PRIMARY KEY, slug TEXT NOT NULL, verdict TEXT NOT NULL CHECK (verdict IN ('like', 'dislike')), game_version TEXT NOT NULL DEFAULT '', loader TEXT NOT NULL DEFAULT '', source TEXT NOT NULL DEFAULT '', timestamp TEXT NOT NULL);
        CREATE INDEX IF NOT EXISTS idx_feedback_slug ON feedback(slug);
        CREATE INDEX IF NOT EXISTS idx_feedback_verdict ON feedback(verdict);
        CREATE TABLE IF NOT EXISTS feedback_tags (feedback_id INTEGER NOT NULL REFERENCES feedback(id) ON DELETE CASCADE, tag TEXT NOT NULL, PRIMARY KEY (feedback_id, tag));
        CREATE INDEX IF NOT EXISTS idx_feedback_tags_tag ON feedback_tags(tag);
        CREATE TABLE IF NOT EXISTS packs (id INTEGER PRIMARY KEY, name TEXT NOT NULL, created_at TEXT NOT NULL);
        CREATE TABLE IF NOT EXISTS pack_mods (pack_id INTEGER NOT NULL REFERENCES packs(id) ON DELETE CASCADE, position INTEGER NOT NULL, mod_slug TEXT NOT NULL, PRIMARY KEY (pack_id, position));
        CREATE INDEX IF NOT EXISTS idx_pack_mods_slug ON pack_mods(mod_slug);")?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn sqlite_round_trip_preserves_relations() {
        let path = std::env::temp_dir().join(format!("rustagent-db-{}.json", std::process::id()));
        let db_path = path.clone();
        let json_path = db_path.display().to_string();
        let _ = std::fs::remove_file(&db_path);
        let mut db = UserDatabase::load(&json_path);
        db.rate(FeedbackRecord {
            slug: "sodium".into(),
            verdict: "like".into(),
            tags: vec!["optimization".into()],
            game_version: "1.21.1".into(),
            loader: "fabric".into(),
            source: "test".into(),
            timestamp: "now".into(),
        });
        db.packs.push(PackRecord {
            name: "test-pack".into(),
            mod_slugs: vec!["sodium".into(), "fabric-api".into()],
            created_at: "now".into(),
        });
        db.save().unwrap();
        let loaded = UserDatabase::load(&json_path);
        assert_eq!(loaded.feedback[0].tags, vec!["optimization"]);
        assert_eq!(loaded.packs[0].mod_slugs, vec!["sodium", "fabric-api"]);
        assert!(serde_json::to_value(&loaded.packs).is_ok());
        let _ = std::fs::remove_file(db_path);
    }
}
