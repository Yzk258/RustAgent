//! 口味相关工具: record_feedback / get_user_profile / recommend_new。

use super::*;

impl super::ToolRegistry {
    pub(super) async fn record_feedback(&self, args: &str) -> Result<serde_json::Value> {
        #[derive(Deserialize)]
        struct Args {
            slug: String,
            verdict: String,
        }
        let a: Args = serde_json::from_str(args)?;
        if a.verdict != "like" && a.verdict != "dislike" {
            bail!("verdict 必须是 like 或 dislike");
        }
        let mut db = crate::storage::database::UserDatabase::load(&self.db_path);
        let tags = match self.modrinth.project(&a.slug).await {
            Ok(p) => crate::pipeline::taste_tags(&p.categories),
            Err(_) => Vec::new(),
        };
        db.rate(crate::storage::database::FeedbackRecord {
            slug: a.slug.clone(),
            verdict: a.verdict.clone(),
            tags: tags.clone(),
            game_version: String::new(),
            loader: String::new(),
            source: "chat".to_string(),
            timestamp: chrono::Local::now().to_rfc3339(),
        });
        db.save()?;
        Ok(json!({
            "status": "已记录",
            "slug": a.slug,
            "verdict": a.verdict,
            "tags": tags,
            "db_summary": db.summary(),
        }))
    }

    pub(super) async fn get_user_profile(&self) -> Result<serde_json::Value> {
        let db = crate::storage::database::UserDatabase::load(&self.db_path);
        Ok(json!({
            "summary": db.summary(),
            "taste_weights": db.tag_weights(),
            "top_tags": db.top_tags(5),
            "recent_packs": db.packs.iter().rev().take(3).collect::<Vec<_>>(),
        }))
    }

    pub(super) async fn recommend_new(&self, args: &str) -> Result<serde_json::Value> {
        #[derive(Deserialize)]
        struct Args {
            game_version: String,
            loader: String,
            #[serde(default)]
            count: Option<u32>,
        }
        let a: Args = serde_json::from_str(args)?;
        let db = crate::storage::database::UserDatabase::load(&self.db_path);
        let ranked =
            crate::pipeline::try_this(&self.modrinth, &db, &a.game_version, &a.loader).await?;
        let take = a.count.unwrap_or(5).clamp(1, 10) as usize;
        let mods: Vec<serde_json::Value> = ranked
            .into_iter()
            .take(take)
            .map(|s| {
                json!({
                    "slug": s.hit.slug,
                    "title": s.hit.title,
                    "description": s.hit.description,
                    "downloads": s.hit.downloads,
                    "categories": s.hit.display_categories,
                    "taste_score": s.score,
                })
            })
            .collect();
        Ok(json!({ "recommendations": mods, "db_summary": db.summary() }))
    }
}
