use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};

#[derive(Serialize, Deserialize, Clone)]
pub struct FeedbackRecord {
    pub slug: String,
    pub verdict: String,
    pub tags: Vec<String>,
    pub game_version: String,
    pub loader: String,
    pub source: String,
    pub timestamp: String,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct PackRecord {
    pub name: String,
    pub mod_slugs: Vec<String>,
    pub created_at: String,
}

#[derive(Serialize, Deserialize, Default, Clone)]
pub struct UserDatabase {
    #[serde(default)]
    pub feedback: Vec<FeedbackRecord>,
    #[serde(default)]
    pub packs: Vec<PackRecord>,
}

impl UserDatabase {
    pub fn load(path: &str) -> Self {
        std::fs::read_to_string(path)
            .ok()
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &str) -> Result<()> {
        if let Some(parent) = std::path::Path::new(path).parent() {
            std::fs::create_dir_all(parent).ok();
        }
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    pub fn rate(&mut self, record: FeedbackRecord) {
        self.feedback.push(record);
    }

    pub fn rated_slugs(&self) -> HashSet<String> {
        self.feedback.iter().map(|f| f.slug.clone()).collect()
    }

    pub fn tag_weights(&self) -> BTreeMap<String, f64> {
        let mut weights: BTreeMap<String, f64> = BTreeMap::new();
        for f in &self.feedback {
            let delta = if f.verdict == "like" { 1.0 } else { -1.0 };
            for tag in &f.tags {
                *weights.entry(tag.clone()).or_insert(0.0) += delta;
            }
        }
        weights
    }

    pub fn top_tags(&self, n: usize) -> Vec<String> {
        let mut pairs: Vec<(String, f64)> = self
            .tag_weights()
            .into_iter()
            .filter(|(_, w)| *w > 0.0)
            .collect();
        pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        pairs.into_iter().take(n).map(|(t, _)| t).collect()
    }

    pub fn summary(&self) -> String {
        let likes = self.feedback.iter().filter(|f| f.verdict == "like").count();
        format!(
            "反馈 {} 条 (喜欢 {} / 不喜欢 {}) | 组包 {} 次",
            self.feedback.len(),
            likes,
            self.feedback.len() - likes,
            self.packs.len()
        )
    }
}
