//! 二进制入口: 只做子命令分发, 其余逻辑在 lib 各模块。
//! - cargo run            → cli::repl (交互式主循环)
//! - cargo run -- selftest → selftest::run
//! - cargo run -- ui       → ui::serve
//! - cargo run -- repair   → tools::repair_pack

use rustagent::prelude::*;
use rustagent::{
    cli, config,
    providers::modrinth,
    selftest,
    tools::{self, ToolRegistry},
    ui,
};

#[tokio::main]
async fn main() -> Result<()> {
    cli::enable_vt();
    let args: Vec<String> = std::env::args().collect();
    let cfg = config::load("config.toml")?;

    if args.len() > 1 && args[1] == "selftest" {
        return selftest::run(&cfg).await;
    }
    if args.len() > 1 && args[1] == "ui" {
        return ui::serve(cfg, "config.toml").await;
    }
    if args.len() > 2 && args[1] == "repair" {
        let client = modrinth::ModrinthClient::new()?;
        let registry = ToolRegistry::new(client, None, &cfg.output.download_dir, cfg.db_path());
        let result = registry
            .execute("repair_pack", &args[2..].join(" "), &tools::TaskCtx::none())
            .await?;
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    if args.len() > 1 {
        bail!(
            "未知子命令 '{}' (可用: selftest / ui / repair; 不带参数进入交互对话)",
            args[1]
        );
    }

    cli::repl(&cfg).await
}
