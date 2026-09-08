use crate::prelude::*;
use std::collections::HashSet;

use crate::providers::modrinth::{Hit, ModrinthClient};
use crate::storage::database::UserDatabase;

pub struct ScoredHit {
    pub hit: Hit,
    pub score: f64,
}

pub fn rank(hits: Vec<Hit>, db: &UserDatabase) -> Vec<ScoredHit> {
    let weights = db.tag_weights();
    let mut scored: Vec<ScoredHit> = hits
        .into_iter()
        .map(|hit| {
            let score: f64 = hit
                .display_categories
                .iter()
                .filter_map(|c| weights.get(c))
                .sum();
            ScoredHit { hit, score }
        })
        .collect();
    // 随机微扰口味分 (-0.5..0.5), 让同分 (尤其空口味时全是 0.0) 的 mod 不再固定按下载量死排,
    // 每次点"换一批"顺序都有变化, 但高口味项仍大概率靠前
    scored.sort_by(|a, b| {
        let sa = a.score + rand::random::<f64>() - 0.5;
        let sb = b.score + rand::random::<f64>() - 0.5;
        sb.partial_cmp(&sa)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(b.hit.downloads.cmp(&a.hit.downloads))
    });
    scored
}

pub async fn try_this(
    client: &ModrinthClient,
    db: &UserDatabase,
    game_version: &str,
    loader: &str,
) -> Result<Vec<ScoredHit>> {
    let tags = db.top_tags(3);
    let mut facets = vec![vec![format!("versions:{game_version}")]];
    if !loader.is_empty() {
        facets.push(vec![format!("categories:{loader}")]);
    }
    if !tags.is_empty() {
        facets.push(tags.iter().map(|t| format!("categories:{t}")).collect());
    }
    let index = if tags.is_empty() {
        "downloads"
    } else {
        "updated"
    };
    // 随机翻页: 0..5 页内随机偏移, 每次点"换一批"都从不同窗口取候选, 避免结果一成不变
    let offset = (rand::random::<u32>() % 5) * 20;
    let hits = client
        .search_with("", Some(facets), 20, offset, index)
        .await?
        .hits;
    let rated = db.rated_slugs();
    let fresh: Vec<Hit> = hits
        .into_iter()
        .filter(|h| !rated.contains(&h.slug))
        .collect();
    if fresh.is_empty() {
        bail!("没有新的可推荐 mod (数据库中已评价过所有候选), 稍后再试");
    }
    Ok(rank(fresh, db))
}

pub async fn dependency_closure(
    client: &ModrinthClient,
    game_version: &str,
    loader: &str,
    seeds: &[String],
    ctx: &crate::tools::TaskCtx,
) -> Result<(Vec<String>, Vec<String>, Vec<String>)> {
    let mut all: Vec<String> = seeds.to_vec();
    let mut auto_added: Vec<String> = Vec::new();
    let mut conflicts: Vec<String> = Vec::new();
    let mut seen_slugs: HashSet<String> = seeds.iter().cloned().collect();
    let mut seen_ids: HashSet<String> = HashSet::new();
    let mut frontier: Vec<String> = seeds.to_vec();

    for _pass in 0..4 {
        if frontier.is_empty() {
            break;
        }
        ctx.check_interrupt()?;
        ctx.report(
            format!(
                "依赖闭包第 {} 轮: 检查 {} 个 mod 的依赖",
                _pass + 1,
                frontier.len()
            ),
            None,
            None,
        );
        let mut next_ids: Vec<String> = Vec::new();
        for slug in &frontier {
            ctx.check_interrupt()?;
            let versions = match client.versions(slug, game_version, loader).await {
                Ok(v) => v,
                Err(e) => {
                    conflicts.push(format!("{slug}: 获取版本失败 ({e:#})"));
                    continue;
                }
            };
            let v = match versions.first() {
                Some(v) => v,
                None => {
                    conflicts.push(format!("{slug}: 没有 {game_version}/{loader} 兼容版本"));
                    continue;
                }
            };
            for dep in &v.dependencies {
                if dep.dependency_type != "required" {
                    continue;
                }
                if let Some(id) = &dep.project_id {
                    if !seen_ids.contains(id) {
                        seen_ids.insert(id.clone());
                        next_ids.push(id.clone());
                    }
                }
            }
        }
        let mut new_frontier = Vec::new();
        for id in next_ids {
            ctx.check_interrupt()?;
            let project = match client.project(&id).await {
                Ok(p) => p,
                Err(_) => {
                    conflicts.push(format!("依赖 {id}: 无法解析"));
                    continue;
                }
            };
            if !seen_slugs.contains(&project.slug) {
                seen_slugs.insert(project.slug.clone());
                all.push(project.slug.clone());
                auto_added.push(project.slug.clone());
                new_frontier.push(project.slug);
            }
        }
        frontier = new_frontier;
    }
    Ok((all, auto_added, conflicts))
}

const LOADER_TAGS: [&str; 5] = ["fabric", "forge", "neoforge", "quilt", "vanilla"];

const KEYWORD_MAP: [(&str, &str); 24] = [
    ("龙世界", "dragon"),
    ("恶龙", "dragon"),
    ("龙", "dragon"),
    ("魔法世界", "magic"),
    ("奇幻", "fantasy"),
    ("魔法", "magic"),
    ("法术", "spell"),
    ("地牢", "dungeon"),
    ("迷宫", "dungeon"),
    ("城堡", "castle"),
    ("中世纪", "medieval"),
    ("探索", "exploration"),
    ("冒险", "adventure"),
    ("科技", "technology"),
    ("工业", "industry"),
    ("性能", "optimization"),
    ("优化", "optimization"),
    ("装饰", "decoration"),
    ("家具", "furniture"),
    ("建筑", "building"),
    ("存储", "storage"),
    ("食物", "food"),
    ("魔物", "monster"),
    ("空岛", "skyblock"),
];

pub fn translate_keyword(q: &str) -> String {
    let s = q.trim().to_lowercase();
    if s.is_empty() {
        return s;
    }
    for (zh, en) in KEYWORD_MAP {
        if s.contains(zh) {
            return en.to_string();
        }
    }
    s
}

pub fn is_valid_game_version(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    !parts.is_empty()
        && parts.len() <= 4
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
}

const VALID_LOADERS: [&str; 4] = ["fabric", "forge", "neoforge", "quilt"];

pub fn is_valid_loader(s: &str) -> bool {
    VALID_LOADERS.contains(&s)
}

/// 单次对话找包数量上限 (CLI /set、Web 预设栏、search_mods clamp 三处共用)
pub const MAX_SEARCH_LIMIT: u32 = 20;

/// 生成 "[界面预设: ...] " 前缀, CLI 的 /set 与 Web 预设栏共用同一注入逻辑。
/// 无任何有效项时返回 None (原样透传消息)。找包数量单次对话上限 20。
pub fn preset_prefix(gv: Option<&str>, loader: Option<&str>, limit: Option<u32>) -> Option<String> {
    let mut parts: Vec<String> = Vec::new();
    if let Some(gv) = gv.map(str::trim).filter(|s| !s.is_empty()) {
        if is_valid_game_version(gv) {
            parts.push(format!("Minecraft 版本 {gv}"));
        }
    }
    if let Some(ld) = loader.map(str::trim).filter(|s| !s.is_empty()) {
        if is_valid_loader(ld) {
            parts.push(format!("{ld} 加载器"));
        }
    }
    if let Some(n) = limit.filter(|n| (1..=MAX_SEARCH_LIMIT).contains(n)) {
        parts.push(format!("候选数量 {n}"));
    }
    if parts.is_empty() {
        None
    } else {
        Some(format!("[界面预设: {}] ", parts.join(" / ")))
    }
}

pub fn taste_tags(categories: &[String]) -> Vec<String> {
    categories
        .iter()
        .filter(|c| !LOADER_TAGS.contains(&c.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_versions_and_loaders() {
        assert!(is_valid_game_version("1.21.1"));
        assert!(!is_valid_game_version("1..21"));
        assert!(!is_valid_game_version("v1.21"));
        assert!(is_valid_loader("fabric"));
        assert!(!is_valid_loader("vanilla"));
    }

    #[test]
    fn preset_prefix_filters_invalid_values() {
        assert_eq!(
            preset_prefix(Some("1.21.1"), Some("fabric"), Some(8)),
            Some("[界面预设: Minecraft 版本 1.21.1 / fabric 加载器 / 候选数量 8] ".into())
        );
        assert_eq!(preset_prefix(Some("bad"), Some("vanilla"), Some(0)), None);
    }
}
