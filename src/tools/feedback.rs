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
        db.append_feedback(crate::storage::database::FeedbackRecord {
            slug: a.slug.clone(),
            verdict: a.verdict.clone(),
            tags: tags.clone(),
            game_version: String::new(),
            loader: String::new(),
            source: "chat".to_string(),
            timestamp: chrono::Local::now().to_rfc3339(),
        })?;
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
        let picked: Vec<_> = ranked.into_iter().take(take).collect();

        // 并发补查每个 mod 的 project 拿 icon_url + gallery 截图 (拼官网链接无需请求, 由 slug 直接构造)。
        // 最多 10 个, 走信号量限流, 1-2 批完成。图标缺失时回退空串, 前端用占位。
        let mut set = tokio::task::JoinSet::new();
        for s in &picked {
            let client = self.modrinth.clone();
            let slug = s.hit.slug.clone();
            set.spawn(async move {
                let p = client.project(&slug).await;
                (slug, p)
            });
        }
        let mut meta: std::collections::HashMap<String, (String, Vec<String>)> =
            std::collections::HashMap::new();
        while let Some(res) = set.join_next().await {
            if let Ok((slug, Ok(p))) = res {
                // gallery 取前 3 张 (避免数据过大, 对话卡片点击展开查看)
                let gallery: Vec<String> = p.gallery.into_iter().take(3).collect();
                meta.insert(slug, (p.icon_url, gallery));
            }
        }

        let mods: Vec<serde_json::Value> = picked
            .into_iter()
            .map(|s| {
                let (icon_url, gallery) = meta.get(&s.hit.slug).cloned().unwrap_or_default();
                json!({
                    "slug": s.hit.slug,
                    "title": s.hit.title,
                    "description": s.hit.description,
                    "downloads": s.hit.downloads,
                    "categories": s.hit.display_categories,
                    "taste_score": s.score,
                    "icon_url": icon_url,
                    "url": format!("https://modrinth.com/mod/{}", s.hit.slug),
                    "gallery": gallery,
                })
            })
            .collect();
        Ok(json!({ "recommendations": mods, "db_summary": db.summary() }))
    }
}
