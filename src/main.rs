mod agent;
mod config;
mod database;
mod history;
mod llm;
mod modrinth;
mod pipeline;
mod tools;

use agent::Agent;
use anyhow::{bail, Result};
use llm::LlmClient;
use std::io::Write;
use std::sync::Arc;
use tokio::io::{AsyncBufReadExt, BufReader};
use tools::ToolRegistry;

use std::io::Read as _;

#[tokio::main]
async fn main() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    let cfg = config::load("config.toml")?;

    if args.len() > 1 && args[1] == "selftest" {
        return selftest(&cfg).await;
    }
    if args.len() > 1 && args[1] == "demo" {
        return pipeline::run_demo(&cfg, args.get(2).map(|s| s.as_str())).await;
    }
    if args.len() > 2 && args[1] == "repair" {
        let modrinth = modrinth::ModrinthClient::new()?;
        let registry = ToolRegistry::new(modrinth, &cfg.output.download_dir, &cfg.db_path());
        let result = registry.execute("repair_pack", &args[2..].join(" ")).await?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }

    let agent = Arc::new(tokio::sync::Mutex::new(new_agent(&cfg).await?));
    let mut reader = BufReader::new(tokio::io::stdin());

    println!("RustAgent v0.2 — MC 模组管理 Agent (测试版)");
    println!("命令: /new 新会话 | /save 保存 | /load 加载 | /stats 数据库与用量 | /quit 退出");
    println!("任务执行中按 Ctrl+C 可打断当前任务 (不会退出程序)");
    println!("示例: 我想要 1.21.1 fabric 的生存整合包, 带点探索和装饰内容\n");

    loop {
        print!("> ");
        std::io::stdout().flush()?;
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        match line.trim() {
            "" => continue,
            "/quit" | "/exit" => break,
            "/new" => {
                *agent.lock().await = new_agent(&cfg).await?;
                println!("已开启新会话");
            }
            "/save" => {
                let ag = agent.lock().await;
                match history::save(&ag) {
                    Ok(p) => println!("已保存: {p}"),
                    Err(e) => eprintln!("[错误] {e:#}"),
                }
            }
            "/load" => {
                let mut ag = agent.lock().await;
                match history::load_latest(&mut ag) {
                    Ok(p) => println!("已加载: {p}"),
                    Err(e) => eprintln!("[错误] {e:#}"),
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
            input => {
                let ag = Arc::clone(&agent);
                let input = input.to_string();
                let mut turn = tokio::spawn(async move {
                    let mut guard = ag.lock().await;
                    guard.run_turn(&input).await
                });
                tokio::select! {
                    res = &mut turn => {
                        match res {
                            Ok(Ok(())) => {}
                            Ok(Err(e)) => eprintln!("[错误] {e:#}"),
                            Err(e) => eprintln!("[错误] 任务异常: {e}"),
                        }
                    }
                    _ = tokio::signal::ctrl_c() => {
                        turn.abort();
                        println!("\n[已打断当前任务]");
                    }
                }
            }
        }
    }
    let ag = agent.lock().await;
    let db = database::UserDatabase::load(&cfg.db_path());
    println!("数据库: {}", db.summary());
    println!("本次会话用量: {}", ag.usage_summary());
    Ok(())
}

async fn new_agent(cfg: &config::Config) -> Result<Agent> {
    let llm = LlmClient::new(cfg.llm.clone())?;
    let modrinth = modrinth::ModrinthClient::new()?;
    let registry = ToolRegistry::new(modrinth, &cfg.output.download_dir, &cfg.db_path());
    Ok(Agent::new(llm, registry, cfg.llm.clone()))
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
    let registry = ToolRegistry::new(mr, &cfg.output.download_dir, &cfg.db_path());
    let val = registry
        .execute(
            "build_modpack",
            r#"{"name":"RustAgent-selftest","game_version":"1.21.1","loader":"fabric","mod_slugs":["sodium","fabric-api"]}"#,
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
