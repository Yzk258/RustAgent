//! `cargo run -- selftest` 子命令: 不耗 LLM token 的自检 (真实访问 Modrinth)。
//! 三步: 搜索 → 四加载器版本获取 → 组包并校验 .mrpack schema。

use crate::config::Config;
use crate::prelude::*;
use crate::providers::modrinth;
use crate::tools::{self, ToolRegistry};
use std::io::Read as _;

pub async fn run(cfg: &Config) -> Result<()> {
    println!("[1/3] Modrinth 搜索测试 (关键词 sodium, 限定 1.21.1 / fabric)");
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

    println!("[2/3] 加载器版本获取测试 (forge / neoforge / quilt, MC 1.21.1)");
    let registry = ToolRegistry::new(mr, None, &cfg.output.download_dir, cfg.db_path());
    for (name, gv) in [
        ("forge", "1.21.1"),
        ("neoforge", "1.21.1"),
        ("quilt", "1.21.1"),
    ] {
        let v = registry.loader_version(name, gv).await?;
        println!("  - {name}: {v}");
    }

    println!("[3/3] 组包测试 (sodium + fabric-api, 含依赖闭包)");
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
        bail!("schema 校验失败: dependencies 必须是对象");
    }
    if !index.get("files").is_some_and(|f| f.is_array()) {
        bail!("schema 校验失败: files 必须是数组");
    }

    println!("\nselftest 通过 (含 .mrpack schema 校验), 请将生成的 .mrpack 拖入启动器验证");
    Ok(())
}
