mod agent;
mod cli;
mod config;
mod database;
mod history;
mod llm;
mod modrinth;
mod pipeline;
mod tools;
mod ui;

use agent::new_agent;
use anyhow::{bail, Result};
use cli::{paint, ACCENT, DIM, GREEN, RED, YELLOW};
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tools::ToolRegistry;

use std::io::Read as _;

#[tokio::main]
async fn main() -> Result<()> {
    cli::enable_vt();
    let args: Vec<String> = std::env::args().collect();
    let cfg = config::load("config.toml")?;

    if args.len() > 1 && args[1] == "selftest" {
        return selftest(&cfg).await;
    }
    if args.len() > 1 && args[1] == "demo" {
        return pipeline::run_demo(&cfg, args.get(2).map(|s| s.as_str())).await;
    }
    if args.len() > 1 && args[1] == "ui" {
        return ui::serve(cfg).await;
    }
    if args.len() > 2 && args[1] == "repair" {
        let modrinth = modrinth::ModrinthClient::new()?;
        let registry = ToolRegistry::new(modrinth, &cfg.output.download_dir, cfg.db_path());
        let result = registry
            .execute("repair_pack", &args[2..].join(" "), &tools::TaskCtx::none())
            .await?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    let interrupt = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let agent = Arc::new(tokio::sync::Mutex::new(
        new_agent(
            &cfg.llm,
            &cfg.output.download_dir,
            &cfg.db_path(),
            interrupt,
        )
        .await?,
    ));
    let mut reader = BufReader::new(tokio::io::stdin());

    // 会话预设 (与 Web UI 预设栏同源): 每条消息注入 [界面预设: ...] 前缀
    let mut preset_gv: Option<String> = None;
    let mut preset_loader: Option<String> = None;
    let mut preset_limit: Option<u32> = None;

    cli::print_banner(&cfg);
    println!();

    loop {
        let mut parts: Vec<String> = Vec::new();
        if let Some(gv) = &preset_gv {
            parts.push(gv.clone());
        }
        if let Some(ld) = &preset_loader {
            parts.push(ld.clone());
        }
        if let Some(n) = preset_limit {
            parts.push(format!("×{n}"));
        }
        let tag = if parts.is_empty() {
            None
        } else {
            Some(parts.join("·"))
        };
        cli::print_prompt(tag.as_deref());
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        match line.trim() {
            "" => continue,
            "/quit" | "/exit" => break,
            "/new" => {
                let interrupt = Arc::new(std::sync::atomic::AtomicBool::new(false));
                *agent.lock().await = new_agent(
                    &cfg.llm,
                    &cfg.output.download_dir,
                    &cfg.db_path(),
                    interrupt,
                )
                .await?;
                println!("{}", paint(GREEN, "✓ 已开启新会话"));
            }
            "/save" => {
                let ag = agent.lock().await;
                match history::save(&ag, &cfg.data_dir()) {
                    Ok(p) => println!("{} {}", paint(GREEN, "✓ 已保存"), paint(DIM, &p)),
                    Err(e) => eprintln!("{}", paint(RED, &format!("✗ {e:#}"))),
                }
            }
            "/load" => {
                let mut ag = agent.lock().await;
                match history::load_latest(&mut ag, &cfg.data_dir()) {
                    Ok(p) => println!("{} {}", paint(GREEN, "✓ 已加载"), paint(DIM, &p)),
                    Err(e) => eprintln!("{}", paint(RED, &format!("✗ {e:#}"))),
                }
            }
            "/stats" => {
                let ag = agent.lock().await;
                let db = database::UserDatabase::load(&cfg.db_path());
                println!("数据库: {}", db.summary());
                let weights = db.tag_weights();
                let mut pairs: Vec<(String, f64)> = weights.into_iter().collect();
                pairs.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
                println!(
                    "口味权重: {}",
                    pairs
                        .iter()
                        .take(8)
                        .map(|(t, w)| format!("{t}={w:+.0}"))
                        .collect::<Vec<_>>()
                        .join("  ")
                );
                println!("本次会话: {}", ag.usage_summary());
            }
            cmd if cmd.starts_with("/set") => {
                let args: Vec<&str> = cmd.split_whitespace().skip(1).collect();
                let apply = |gv: &mut Option<String>,
                             ld: &mut Option<String>,
                             n: &mut Option<u32>,
                             args: &[&str]| {
                    if args.is_empty() {
                        println!("{}", paint(DIM, "用法: /set 版本=1.21.1 加载器=fabric 数量=10 (键: 版本/加载器/数量)"));
                    }
                    for token in args {
                        let Some((key, val)) = token.split_once('=') else {
                            eprintln!("{}", paint(RED, "✗ 格式: /set 版本=1.21.1 加载器=fabric 数量=10 (键: 版本/加载器/数量)"));
                            continue;
                        };
                        match key {
                            "版本" | "version" | "v" => {
                                if pipeline::is_valid_game_version(val) {
                                    *gv = Some(val.to_string());
                                } else {
                                    eprintln!(
                                        "{}",
                                        paint(RED, &format!("✗ 无效版本 '{val}', 应类似 1.21.1"))
                                    );
                                }
                            }
                            "加载器" | "loader" | "l" => {
                                if pipeline::is_valid_loader(val) {
                                    *ld = Some(val.to_string());
                                } else {
                                    eprintln!("{}", paint(RED, &format!("✗ 无效加载器 '{val}', 可选: fabric / forge / neoforge / quilt")));
                                }
                            }
                            "数量" | "limit" | "n" => match val.parse::<u32>() {
                                Ok(x) if (1..=20).contains(&x) => *n = Some(x),
                                _ => eprintln!(
                                    "{}",
                                    paint(RED, "✗ 数量需为 1-20 的整数 (单次对话上限 20)")
                                ),
                            },
                            _ => eprintln!(
                                "{}",
                                paint(
                                    RED,
                                    &format!("✗ 未知预设项 '{key}', 可选: 版本 / 加载器 / 数量")
                                )
                            ),
                        }
                    }
                };
                apply(&mut preset_gv, &mut preset_loader, &mut preset_limit, &args);
                let mut parts: Vec<String> = Vec::new();
                if let Some(gv) = &preset_gv {
                    parts.push(format!("版本={gv}"));
                }
                if let Some(ld) = &preset_loader {
                    parts.push(format!("加载器={ld}"));
                }
                if let Some(n) = preset_limit {
                    parts.push(format!("数量={n}"));
                }
                if parts.is_empty() {
                    println!("当前预设: 无 (消息将原样发送)");
                } else {
                    println!(
                        "{} {}",
                        paint(GREEN, "✓ 当前预设:"),
                        paint(ACCENT, &parts.join(" "))
                    );
                }
            }
            input => {
                let ag = Arc::clone(&agent);
                // 注入会话预设前缀 (与 Web 预设栏同源逻辑), 让 agent 直接采用不再追问
                let input = match pipeline::preset_prefix(
                    preset_gv.as_deref(),
                    preset_loader.as_deref(),
                    preset_limit,
                ) {
                    Some(p) => format!("{p}{input}"),
                    None => input.to_string(),
                };
                cli::print_busy_hint();
                let mut turn = tokio::spawn(async move {
                    let mut guard = ag.lock().await;
                    guard.run_turn(&input).await
                });
                tokio::select! {
                    res = &mut turn => {
                        match res {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => eprintln!("{}", paint(RED, &format!("✗ {e:#}"))),
                            Err(e) => eprintln!("{}", paint(RED, &format!("✗ 任务异常: {e}"))),
                        }
                    }
                    _ = tokio::signal::ctrl_c() => {
                        turn.abort();
                        println!("\n{}", paint(YELLOW, "⏸ 已打断当前任务"));
                    }
                }
                // 每轮对话默认自动保存 (打断/出错也保留已有内容), 同一会话覆盖写同一文件
                {
                    let mut ag = agent.lock().await;
                    match history::auto_save(&mut ag, &cfg.data_dir()) {
                        Ok(p) if !p.is_empty() => {
                            println!("{}", paint(DIM, &format!("  ✓ 已自动保存 {p}")))
                        }
                        Ok(_) => {}
                        Err(e) => eprintln!("{}", paint(RED, &format!("✗ 自动保存失败: {e:#}"))),
                    }
                }
            }
        }
    }
    let ag = agent.lock().await;
    let db = database::UserDatabase::load(&cfg.db_path());
    println!("{}", paint(DIM, "── 会话结束 ──────────────────────"));
    println!("数据库: {}", db.summary());
    println!("本次会话用量: {}", ag.usage_summary());
    Ok(())
}

async fn selftest(cfg: &config::Config) -> Result<()> {
    println!("[1/2] Modrinth 搜索测试 (关键词 sodium, 限定 1.21.1 / fabric)");
    let mr = modrinth::ModrinthClient::new()?;
    let resp = mr
        .search(
            "sodium",
            Some(vec![
                vec!["versions:1.21.1".to_string()],
                vec!["categories:fabric".to_string()],
            ]),
            3,
            "relevance",
        )
        .await?;
    for h in &resp.hits {
        println!("  - {} ({}) | {} 下载", h.title, h.slug, h.downloads);
    }
    if resp.hits.is_empty() {
        bail!("搜索无结果, facets 过滤可能有问题");
    }

    println!("[2/2] 组包测试 (sodium + fabric-api, 含依赖闭包)");
    let registry = ToolRegistry::new(mr, &cfg.output.download_dir, cfg.db_path());
    let val = registry
        .execute(
            "build_modpack",
            r#"{"name":"RustAgent-selftest","game_version":"1.21.1","loader":"fabric","mod_slugs":["sodium","fabric-api"]}"#,
            &tools::TaskCtx::none(),
        )
        .await?;
    println!("{}", serde_json::to_string_pretty(&val)?);

    let out_path = val
        .get("output_path")
        .and_then(|v| v.as_str())
        .ok_or_else(|| anyhow::anyhow!("组包结果缺少 output_path"))?;
    let pack_file = std::fs::File::open(out_path)?;
    let mut archive = zip::ZipArchive::new(pack_file)?;
    let mut index_json = String::new();
    archive
        .by_name("modrinth.index.json")?
        .read_to_string(&mut index_json)?;
    let index: serde_json::Value = serde_json::from_str(&index_json)?;
    if !index.get("dependencies").is_some_and(|d| d.is_object()) {
        bail!("schema 校验失败: dependencies 必须是对象 (PCL2 兼容)");
    }
    if !index.get("files").is_some_and(|f| f.is_array()) {
        bail!("schema 校验失败: files 必须是数组");
    }

    println!("\nselftest 通过 (含 .mrpack schema 校验), 请将生成的 .mrpack 拖入启动器验证");
    Ok(())
}
