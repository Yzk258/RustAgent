use anyhow::{bail, Result};
use serde_json::json;
use std::collections::HashSet;

use crate::config::Config;
use crate::database::{FeedbackRecord, PackRecord, UserDatabase};
use crate::modrinth::{Hit, ModrinthClient};
use crate::tools::ToolRegistry;

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

pub async fn suggest(
    client: &ModrinthClient,
    query: &str,
    game_version: &str,
    loader: &str,
    limit: u32,
) -> Result<Vec<Hit>> {
    let mut facets = vec![vec![format!("versions:{game_version}")]];
    if !loader.is_empty() {
        facets.push(vec![format!("categories:{loader}")]);
    }
    Ok(client
        .search(query, Some(facets), limit, "relevance")
        .await?
        .hits)
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
    if let Some(n) = limit.filter(|n| (1..=20).contains(n)) {
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

fn prompt(text: &str) -> String {
    print!("{text}");
    use std::io::Write;
    std::io::stdout().flush().ok();
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).ok();
    line.trim().to_string()
}

fn print_candidates(ranked: &[ScoredHit]) {
    println!("--- 候选列表 (按你的口味排序) ---");
    for (i, s) in ranked.iter().enumerate() {
        let tag = if s.score > 0.0 {
            format!("口味+{}", s.score)
        } else if s.score < 0.0 {
            format!("口味{}", s.score)
        } else {
            "新".to_string()
        };
        let desc: String = s.hit.description.chars().take(50).collect();
        println!(
            "{}. [{}] {} ({}) | 下载 {}",
            i + 1,
            tag,
            s.hit.title,
            s.hit.slug,
            s.hit.downloads
        );
        println!("   {}", desc);
        println!("   分类: {}", s.hit.display_categories.join(", "));
    }
}

pub async fn run_demo(cfg: &Config, mode: Option<&str>) -> Result<()> {
    let db_path = cfg.db_path();
    let mut db = UserDatabase::load(&db_path);
    let client = ModrinthClient::new()?;
    let game_version = "1.21.1";
    let loader = "fabric";

    if mode == Some("trythis") {
        println!("=== 试试这个 (基于 {} ) ===", db.summary());
        match try_this(&client, &db, game_version, loader).await {
            Ok(ranked) => {
                let top: Vec<ScoredHit> = ranked.into_iter().take(5).collect();
                print_candidates(&top);
            }
            Err(e) => println!("[提示] {e:#}"),
        }
        return Ok(());
    }

    println!("=== RustAgent 组包逻辑演示 (无 LLM) ===");
    println!("数据库状态: {}", db.summary());

    let input_version = prompt(&format!("MC 版本 [{game_version}]: "));
    let game_version = if input_version.is_empty() {
        game_version
    } else if is_valid_game_version(&input_version) {
        &input_version
    } else {
        bail!(
            "'{input_version}' 不是有效的游戏版本 — 应类似 1.21.1 (只需要版本号, 模组包主题填在下一个提示里)"
        );
    };
    let input_loader = prompt(&format!("加载器 [{loader}]: "));
    let loader = if input_loader.is_empty() {
        loader
    } else if VALID_LOADERS.contains(&input_loader.as_str()) {
        &input_loader
    } else {
        bail!(
            "加载器 '{}' 不支持, 可选: fabric / forge / neoforge / quilt",
            input_loader
        );
    };
    let query_raw = prompt("主题关键词: ");
    if query_raw.is_empty() {
        bail!("未输入主题关键词");
    }
    let query = translate_keyword(&query_raw);
    if query != query_raw {
        println!("已转换关键词: {query_raw} → {query}");
    }

    let hits = suggest(&client, &query, game_version, loader, 8).await?;
    if hits.is_empty() {
        bail!(
            "搜索无结果 (已尝试英文关键词 '{query}') — 试试其他英文词, 如 dragon / fantasy / magic / dungeon"
        );
    }
    let ranked = rank(hits, &db);
    print_candidates(&ranked);
    println!("--- 逐个筛选: y=加入+喜欢 | n=跳过+不喜欢 | s=跳过不评价 | q=结束 ---");

    let mut keep: Vec<String> = Vec::new();
    for (i, s) in ranked.iter().enumerate() {
        if keep.len() >= 5 {
            println!("已达上限 5 个主 mod");
            break;
        }
        let answer = prompt(&format!("  [{}] {} > ", i + 1, s.hit.slug));
        let verdict = match answer.as_str() {
            "y" => Some("like"),
            "n" => Some("dislike"),
            "s" => None,
            "q" => break,
            _ => None,
        };
        if answer == "q" {
            break;
        }
        if let Some(v) = verdict {
            db.rate(FeedbackRecord {
                slug: s.hit.slug.clone(),
                verdict: v.to_string(),
                tags: taste_tags(&s.hit.display_categories),
                game_version: game_version.to_string(),
                loader: loader.to_string(),
                source: "demo".to_string(),
                timestamp: chrono::Local::now().to_rfc3339(),
            });
            if v == "like" {
                keep.push(s.hit.slug.clone());
            }
        }
    }

    if keep.is_empty() {
        bail!("没有选择任何 mod, 演示结束");
    }
    println!("\n已选主 mod: {}", keep.join(", "));
    println!("\n组包中 (含依赖闭包解析与冲突元检测)...");

    let registry = ToolRegistry::new(
        ModrinthClient::new()?,
        &cfg.output.download_dir,
        cfg.db_path(),
    );
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let pack_name = format!("demo-{ts}");
    let args = json!({
        "name": pack_name,
        "game_version": game_version,
        "loader": loader,
        "mod_slugs": keep,
    })
    .to_string();
    // demo 也走真实进展通道, 终端实时显示 "正在收集 mod x (i/n)"
    let (ptx, mut prx) = tokio::sync::mpsc::unbounded_channel::<crate::tools::ProgressUpdate>();
    let printer = tokio::spawn(async move {
        while let Some(u) = prx.recv().await {
            println!("  ⏳ {}", u.text);
        }
    });
    let ctx = crate::tools::TaskCtx {
        progress: Some(ptx),
        interrupt: None,
    };
    let result = registry.execute("build_modpack", &args, &ctx).await?;
    drop(ctx); // 释放进展通道, 让打印任务自然结束
    let _ = printer.await;

    if let Some(arr) = result.get("auto_added").and_then(|v| v.as_array()) {
        if arr.is_empty() {
            println!("无需自动补全前置依赖");
        } else {
            let names: Vec<String> = arr
                .iter()
                .map(|v| v.as_str().unwrap_or_default().to_string())
                .collect();
            println!("自动补全前置依赖 {} 个: {}", arr.len(), names.join(", "));
        }
    }
    println!("{}", serde_json::to_string_pretty(&result)?);

    db.packs.push(PackRecord {
        name: pack_name,
        mod_slugs: keep,
        created_at: chrono::Local::now().to_rfc3339(),
    });
    db.save()?;

    let weights = db.tag_weights();
    let mut pairs: Vec<(String, f64)> = weights.into_iter().collect();
    pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    println!("\n数据库已更新: {db_path} ({})", db.summary());
    println!(
        "当前口味权重: {}",
        pairs
            .iter()
            .take(6)
            .map(|(t, w)| format!("{t}={w:+.0}"))
            .collect::<Vec<_>>()
            .join("  ")
    );
    println!("\n提示: 再次运行 `cargo run -- demo` 感受排序变化, 或 `cargo run -- demo trythis` 测试推荐");
    Ok(())
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
