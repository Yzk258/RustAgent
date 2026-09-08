//! search_mods 工具: Modrinth 关键词搜索 (中文自动转英文) + CurseForge 独占候选回退。

use super::*;

impl super::ToolRegistry {
    pub(super) async fn search_mods(&self, args: &str) -> Result<serde_json::Value> {
        let a: SearchArgs = serde_json::from_str(args)?;
        let query = crate::pipeline::translate_keyword(&a.query);
        let mut facets: Vec<Vec<String>> = Vec::new();
        if let Some(gv) = &a.game_version {
            facets.push(vec![format!("versions:{gv}")]);
        }
        if let Some(l) = &a.loader {
            facets.push(vec![format!("categories:{l}")]);
        }
        let resp = self
            .modrinth
            .search(
                &query,
                if facets.is_empty() {
                    None
                } else {
                    Some(facets)
                },
                a.limit
                    .unwrap_or(8)
                    .clamp(1, crate::pipeline::MAX_SEARCH_LIMIT),
                "relevance",
            )
            .await?;
        let mut mods: Vec<serde_json::Value> = resp
            .hits
            .iter()
            .map(|h| {
                let recent: Vec<String> = h.versions.iter().rev().take(4).cloned().collect();
                json!({
                    "slug": h.slug,
                    "title": h.title,
                    "author": h.author,
                    "downloads": h.downloads,
                    "description": h.description,
                    "categories": h.display_categories,
                    "recent_game_versions": recent,
                })
            })
            .collect();
        // Modrinth 无结果且开启 CF 支持时: 用查询词拼 slug 点名试一次 CurseForge
        // (仅 ASCII 查询有效; cfwidget 无搜索能力, 命中即视为 CF 独占候选)
        if mods.is_empty() {
            if let (Some(cf), true) = (&self.cf, a.query.trim().is_ascii()) {
                let slug = crate::providers::curseforge::slugify(&a.query);
                if !slug.is_empty() {
                    if let Ok(p) = cf.lookup(&slug).await {
                        mods.push(json!({
                            "slug": slug,
                            "source": "curseforge",
                            "title": p.title,
                            "description": p.summary,
                            "categories": p.categories,
                            "note": "CurseForge 独占候选, 组包时用 build_modpack 的 cf_mods 参数",
                        }));
                    }
                }
            }
        }
        Ok(json!({ "query_used": query, "total_hits": resp.total_hits, "mods": mods }))
    }
}
