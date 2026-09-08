//! ╔══════════════════════════════════════════════════════════════════╗
//! ║  run_demo —— RustAgent 无 LLM 组包流水线 · 退役纪念版             ║
//! ║  2026-09-08 从 src/pipeline.rs 光荣退役, 本文件不参与编译,        ║
//! ║  与本体无任何联系, 仅作历史留念。                                 ║
//! ╚══════════════════════════════════════════════════════════════════╝
//!
//! 【生平】
//! run_demo 生于项目早期 (v0.1, "demo测试版" 即以它命名) —— 那时 LLM
//! 工具调用还没有接进来, 它就是项目的全部: 手动输入版本/加载器/主题关键词,
//! 用纯 Rust 规则跑通 "搜索 → 口味排序 → 逐个 y/n 筛选 → 依赖闭包 → 组包
//! → 写库" 全流程。LLM agent 接入后, 它退居二线做链路验证与课堂演示。
//!
//! 【退休原因】
//! - 链路验证被 selftest 取代 (更完整: 多了四加载器版本获取与 .mrpack
//!   schema 校验, 还免交互);
//! - "试试这个"演示被 Web UI 推荐卡片取代 (体验好得多);
//! - 每次改造 TaskCtx / 进展通道 / 加载器元数据, 它都得跟着同步, 维护成本
//!   大于价值;
//! - 占了 pipeline.rs 40% 的体积, 让纯规则文件显得臃肿。
//!
//! 【如欲复活】
//! 把本文件内容放回 src/demo.rs, 在 lib.rs 登记 `pub mod demo;`,
//! main.rs 加一行分发 `if args[1] == "demo" { return demo::run(...).await; }`。
//! 它生前依赖 (现均在): pipeline::{rank, try_this, translate_keyword,
//! is_valid_game_version, is_valid_loader, taste_tags},
//! storage::database::{UserDatabase, FeedbackRecord, PackRecord},
//! tools::{ToolRegistry, TaskCtx, ProgressUpdate}, providers::modrinth。
//!
//! 安息吧, 你教会了这台机器怎么不靠 AI 也能组出一个整合包。⛏

// ============================================================================
// 以下为退役时原样保留的代码 (依赖类型参考上面的"如欲复活"清单)
// ============================================================================

#[allow(dead_code)]
fn _history_docs() {}

// /// 带 facets 的 Modrinth 搜索封装 (退役时全项目仅 demo 使用)
// pub async fn suggest(
//     client: &ModrinthClient,
//     query: &str,
//     game_version: &str,
//     loader: &str,
//     limit: u32,
// ) -> Result<Vec<Hit>> {
//     let mut facets = vec![vec![format!("versions:{game_version}")]];
//     if !loader.is_empty() {
//         facets.push(vec![format!("categories:{loader}")]);
//     }
//     Ok(client
//         .search(query, Some(facets), limit, "relevance")
//         .await?
//         .hits)
// }
//
// fn prompt(text: &str) -> String {
//     print!("{text}");
//     use std::io::Write;
//     std::io::stdout().flush().ok();
//     let mut line = String::new();
//     std::io::stdin().read_line(&mut line).ok();
//     line.trim().to_string()
// }
//
// fn print_candidates(ranked: &[ScoredHit]) {
//     println!("--- 候选列表 (按你的口味排序) ---");
//     for (i, s) in ranked.iter().enumerate() {
//         let tag = if s.score > 0.0 {
//             format!("口味+{}", s.score)
//         } else if s.score < 0.0 {
//             format!("口味{}", s.score)
//         } else {
//             "新".to_string()
//         };
//         let desc: String = s.hit.description.chars().take(50).collect();
//         println!(
//             "{}. [{}] {} ({}) | 下载 {}",
//             i + 1,
//             tag,
//             s.hit.title,
//             s.hit.slug,
//             s.hit.downloads
//         );
//         println!("   {}", desc);
//         println!("   分类: {}", s.hit.display_categories.join(", "));
//     }
// }
//
// pub async fn run_demo(cfg: &Config, mode: Option<&str>) -> Result<()> {
//     let db_path = cfg.db_path();
//     let mut db = UserDatabase::load(&db_path);
//     let client = ModrinthClient::new()?;
//     let game_version = "1.21.1";
//     let loader = "fabric";
//
//     if mode == Some("trythis") {
//         println!("=== 试试这个 (基于 {} ) ===", db.summary());
//         match try_this(&client, &db, game_version, loader).await {
//             Ok(ranked) => {
//                 let top: Vec<ScoredHit> = ranked.into_iter().take(5).collect();
//                 print_candidates(&top);
//             }
//             Err(e) => println!("[提示] {e:#}"),
//         }
//         return Ok(());
//     }
//
//     println!("=== RustAgent 组包逻辑演示 (无 LLM) ===");
//     println!("数据库状态: {}", db.summary());
//
//     let input_version = prompt(&format!("MC 版本 [{game_version}]: "));
//     let game_version = if input_version.is_empty() {
//         game_version
//     } else if is_valid_game_version(&input_version) {
//         &input_version
//     } else {
//         bail!(
//             "'{input_version}' 不是有效的游戏版本 — 应类似 1.21.1 (只需要版本号, 模组包主题填在下一个提示里)"
//         );
//     };
//     let input_loader = prompt(&format!("加载器 [{loader}]: "));
//     let loader = if input_loader.is_empty() {
//         loader
//     } else if VALID_LOADERS.contains(&input_loader.as_str()) {
//         &input_loader
//     } else {
//         bail!(
//             "加载器 '{}' 不支持, 可选: fabric / forge / neoforge / quilt",
//             input_loader
//         );
//     };
//     let query_raw = prompt("主题关键词: ");
//     if query_raw.is_empty() {
//         bail!("未输入主题关键词");
//     }
//     let query = translate_keyword(&query_raw);
//     if query != query_raw {
//         println!("已转换关键词: {query_raw} → {query}");
//     }
//
//     let hits = suggest(&client, &query, game_version, loader, 8).await?;
//     if hits.is_empty() {
//         bail!(
//             "搜索无结果 (已尝试英文关键词 '{query}') — 试试其他英文词, 如 dragon / fantasy / magic / dungeon"
//         );
//     }
//     let ranked = rank(hits, &db);
//     print_candidates(&ranked);
//     println!("--- 逐个筛选: y=加入+喜欢 | n=跳过+不喜欢 | s=跳过不评价 | q=结束 ---");
//
//     let mut keep: Vec<String> = Vec::new();
//     for (i, s) in ranked.iter().enumerate() {
//         if keep.len() >= 5 {
//             println!("已达上限 5 个主 mod");
//             break;
//         }
//         let answer = prompt(&format!("  [{}] {} > ", i + 1, s.hit.slug));
//         let verdict = match answer.as_str() {
//             "y" => Some("like"),
//             "n" => Some("dislike"),
//             "s" => None,
//             "q" => break,
//             _ => None,
//         };
//         if answer == "q" {
//             break;
//         }
//         if let Some(v) = verdict {
//             db.rate(FeedbackRecord {
//                 slug: s.hit.slug.clone(),
//                 verdict: v.to_string(),
//                 tags: taste_tags(&s.hit.display_categories),
//                 game_version: game_version.to_string(),
//                 loader: loader.to_string(),
//                 source: "demo".to_string(),
//                 timestamp: chrono::Local::now().to_rfc3339(),
//             });
//             if v == "like" {
//                 keep.push(s.hit.slug.clone());
//             }
//         }
//     }
//
//     if keep.is_empty() {
//         bail!("没有选择任何 mod, 演示结束");
//     }
//     println!("\n已选主 mod: {}", keep.join(", "));
//     println!("\n组包中 (含依赖闭包解析与冲突元检测)...");
//
//     let registry = ToolRegistry::new(
//         ModrinthClient::new()?,
//         cfg.curseforge
//             .enabled
//             .then(crate::providers::curseforge::CfClient::new),
//         &cfg.output.download_dir,
//         cfg.db_path(),
//     );
//     let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
//     let pack_name = format!("demo-{ts}");
//     let args = json!({
//         "name": pack_name,
//         "game_version": game_version,
//         "loader": loader,
//         "mod_slugs": keep,
//     })
//     .to_string();
//     // demo 也走真实进展通道, 终端实时显示 "正在收集 mod x (i/n)"
//     let (ptx, mut prx) = tokio::sync::mpsc::unbounded_channel::<crate::tools::ProgressUpdate>();
//     let printer = tokio::spawn(async move {
//         while let Some(u) = prx.recv().await {
//             println!("  ⏳ {}", u.text);
//         }
//     });
//     let ctx = crate::tools::TaskCtx {
//         progress: Some(ptx),
//         interrupt: None,
//     };
//     let result = registry.execute("build_modpack", &args, &ctx).await?;
//     drop(ctx); // 释放进展通道, 让打印任务自然结束
//     let _ = printer.await;
//
//     if let Some(arr) = result.get("auto_added").and_then(|v| v.as_array()) {
//         if arr.is_empty() {
//             println!("无需自动补全前置依赖");
//         } else {
//             let names: Vec<String> = arr
//                 .iter()
//                 .map(|v| v.as_str().unwrap_or_default().to_string())
//                 .collect();
//             println!("自动补全前置依赖 {} 个: {}", arr.len(), names.join(", "));
//         }
//     }
//     println!("{}", serde_json::to_string_pretty(&result)?);
//
//     db.packs.push(PackRecord {
//         name: pack_name,
//         mod_slugs: keep,
//         created_at: chrono::Local::now().to_rfc3339(),
//     });
//     db.save()?;
//
//     let weights = db.tag_weights();
//     let mut pairs: Vec<(String, f64)> = weights.into_iter().collect();
//     pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
//     println!("\n数据库已更新: {db_path} ({})", db.summary());
//     println!(
//         "当前口味权重: {}",
//         pairs
//             .iter()
//             .take(6)
//             .map(|(t, w)| format!("{t}={w:+.0}"))
//             .collect::<Vec<_>>()
//             .join("  ")
//     );
//     println!("\n提示: 再次运行 `cargo run -- demo` 感受排序变化, 或 `cargo run -- demo trythis` 测试推荐");
//     Ok(())
// }
