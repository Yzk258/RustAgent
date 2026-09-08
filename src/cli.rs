//! CLI 模块: 交互式 REPL 主循环 + 视觉样式 (仿 opencode / claude code 风格:
//! 圆角边框横幅、accent 提示符、彩色状态行)。零依赖 ANSI, Windows 启动时启用 VT 处理。

use crate::agent::new_agent;
use crate::config::Config;
use crate::prelude::*;
use std::io::Write;
use tokio::io::{AsyncBufReadExt, BufReader};

pub const DIM: &str = "\x1b[2m";
pub const BOLD: &str = "\x1b[1m";
pub const ACCENT: &str = "\x1b[96m";
pub const GREEN: &str = "\x1b[32m";
pub const RED: &str = "\x1b[31m";
pub const YELLOW: &str = "\x1b[33m";
pub const RESET: &str = "\x1b[0m";

pub fn paint(color: &str, s: &str) -> String {
    format!("{color}{s}{RESET}")
}

/// Windows 10+ 控制台默认不解析 ANSI, 手动开启 VT 处理; 已是 Windows Terminal 等则幂等
#[cfg(windows)]
pub fn enable_vt() {
    use std::os::windows::io::AsRawHandle;
    const ENABLE_VIRTUAL_TERMINAL_PROCESSING: u32 = 0x0004;
    extern "system" {
        fn GetConsoleMode(h: isize, mode: *mut u32) -> i32;
        fn SetConsoleMode(h: isize, mode: u32) -> i32;
    }
    unsafe {
        let h = std::io::stdout().as_raw_handle() as isize;
        let mut mode = 0u32;
        if GetConsoleMode(h, &mut mode) != 0 {
            SetConsoleMode(h, mode | ENABLE_VIRTUAL_TERMINAL_PROCESSING);
        }
    }
}

#[cfg(not(windows))]
pub fn enable_vt() {}

/// 终端显示宽度: unicode-width 权威实现 (East Asian Width)。
/// 注意: 横幅内避免 East Asian Ambiguous 装饰字符 (如 ⛏), 否则不同终端
/// 渲染宽度不一致会导致边框错位; 模糊字符只用在无需对齐的行。
fn display_width(s: &str) -> usize {
    use unicode_width::UnicodeWidthStr;
    s.width()
}

/// 启动横幅: accent 圆角边框 + 加粗标题 + dim 副标题
pub fn print_banner(cfg: &Config) {
    let title = format!("RustAgent v{} · 测试版", env!("CARGO_PKG_VERSION"));
    let sub = format!("模型 {} · mod 数据来自 Modrinth", cfg.llm.model);
    let inner = display_width(&title).max(display_width(&sub));
    let line = "─".repeat(inner + 2);

    println!("{}", paint(ACCENT, &format!("╭{line}╮")));
    let pad = " ".repeat(inner - display_width(&title));
    println!(
        "{} {}{} {}",
        paint(ACCENT, "│"),
        paint(BOLD, &title),
        pad,
        paint(ACCENT, "│")
    );
    let pad = " ".repeat(inner - display_width(&sub));
    println!(
        "{} {}{} {}",
        paint(ACCENT, "│"),
        paint(DIM, &sub),
        pad,
        paint(ACCENT, "│")
    );
    println!("{}", paint(ACCENT, &format!("╰{line}╯")));

    println!(
        "{}",
        paint(
            DIM,
            "  ⛏ /new 新会话  /save 保存  /load 加载  /set 预设  /stats 统计  /quit 退出"
        )
    );
    println!(
        "{}",
        paint(
            DIM,
            "  每轮对话自动保存 · Ctrl+C 打断当前任务 (不退出) · 示例: 我想要 1.21.1 fabric 的生存整合包"
        )
    );
}

/// 提示符: 有预设时显示 dim 标签, 如 [1.21.1·fabric·10] ❯
pub fn print_prompt(preset_tag: Option<&str>) {
    match preset_tag {
        Some(tag) => print!("{} {} ", paint(DIM, &format!("[{tag}]")), paint(BOLD, "❯")),
        None => print!("{} ", paint(BOLD, "❯")),
    }
    let _ = std::io::stdout().flush();
}

/// 任务开始提示 (等待 LLM 首个事件期间给用户反馈)
pub fn print_busy_hint() {
    println!("{}", paint(DIM, "  ✻ 处理中, Ctrl+C 可打断"));
}

/// 长任务实时进展渲染: 数值进度画 ▰▱ 条, 同一行原地刷新 (\r + 清行)。
/// 零依赖 ANSI 而非 indicatif: REPL Ctrl+C 直接 abort 打印任务, 自绘行只是
/// 停在原地不会残留刷新; 三方库的全局 draw target 在该场景下会持续抢占 stdout。
#[derive(Default)]
pub struct ProgressPrinter {
    active: bool,
}

impl ProgressPrinter {
    pub fn new() -> Self {
        Self::default()
    }

    fn clear(&mut self) {
        if self.active {
            let _ = write!(std::io::stdout(), "\r\x1b[2K");
            self.active = false;
        }
    }

    /// 进展事件: 原地覆盖刷新同一行; 有 current/total 时附进度条
    pub fn progress(&mut self, text: &str, current: Option<u64>, total: Option<u64>) {
        let bar = match (current, total) {
            (Some(c), Some(t)) if t > 0 => {
                const W: usize = 20;
                let filled = ((c.min(t) as f64 / t as f64) * W as f64).round() as usize;
                format!("{}{} {c}/{t} ", "▰".repeat(filled), "▱".repeat(W - filled))
            }
            _ => String::new(),
        };
        self.clear();
        let _ = write!(std::io::stdout(), "  {} {bar}{text}", paint(YELLOW, "⏳"));
        let _ = std::io::stdout().flush();
        self.active = true;
    }

    /// 普通行 (工具开始/结束): 先清掉进展行, 再整行打印
    pub fn line(&mut self, s: &str) {
        self.clear();
        println!("{s}");
    }

    /// 多行文本 (最终回复, 无流式增量的兜底路径)
    pub fn text(&mut self, s: &str) {
        self.clear();
        println!("\n{s}\n");
    }

    /// 流式回复增量: 逐片段直接输出 (打字机效果), 首个片段前空一行分隔
    pub fn delta(&mut self, s: &str, streaming: &mut bool) {
        self.clear();
        if !*streaming {
            println!();
            *streaming = true;
        }
        let _ = write!(std::io::stdout(), "{s}");
        let _ = std::io::stdout().flush();
    }

    /// 流式回复结束: 已逐字输出则仅换行收尾, 否则完整打印 (无增量时的兜底)
    pub fn reply_end(&mut self, full: &str, streaming: bool) {
        if streaming {
            println!("\n");
        } else {
            self.text(full);
        }
    }
}

/// 交互式 REPL 主循环 (cargo run 不带子命令时进入)。
/// 处理 /new /save /load /stats /set /quit 与普通对话轮; 每轮 Ctrl+C 可打断,
/// 轮末自动保存。视觉输出全部用本模块的样式函数。
pub async fn repl(cfg: &Config) -> Result<()> {
    let interrupt = Arc::new(AtomicBool::new(false));
    let agent = Arc::new(tokio::sync::Mutex::new(
        new_agent(cfg, interrupt.clone()).await?,
    ));
    let mut reader = BufReader::new(tokio::io::stdin());

    // 会话预设 (与 Web UI 预设栏同源): 每条消息注入 [界面预设: ...] 前缀
    let mut preset_gv: Option<String> = None;
    let mut preset_loader: Option<String> = None;
    let mut preset_limit: Option<u32> = None;

    print_banner(cfg);
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
        print_prompt(tag.as_deref());
        let mut line = String::new();
        let n = reader.read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        match line.trim() {
            "" => continue,
            "/quit" => break,
            "/new" => {
                *agent.lock().await = new_agent(cfg, Arc::new(AtomicBool::new(false))).await?;
                println!("{}", paint(GREEN, "✓ 已开启新会话"));
            }
            "/save" => {
                let ag = agent.lock().await;
                match crate::storage::history::save(&ag, &cfg.data_dir()) {
                    Ok(p) => println!("{} {}", paint(GREEN, "✓ 已保存"), paint(DIM, &p)),
                    Err(e) => eprintln!("{}", paint(RED, &format!("✗ {e:#}"))),
                }
            }
            "/load" => {
                let mut ag = agent.lock().await;
                match crate::storage::history::load_latest(&mut ag, &cfg.data_dir()) {
                    Ok(p) => println!("{} {}", paint(GREEN, "✓ 已加载"), paint(DIM, &p)),
                    Err(e) => eprintln!("{}", paint(RED, &format!("✗ {e:#}"))),
                }
            }
            "/stats" => {
                let ag = agent.lock().await;
                let db = crate::storage::database::UserDatabase::load(&cfg.db_path());
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
                                if crate::pipeline::is_valid_game_version(val) {
                                    *gv = Some(val.to_string());
                                } else {
                                    eprintln!(
                                        "{}",
                                        paint(RED, &format!("✗ 无效版本 '{val}', 应类似 1.21.1"))
                                    );
                                }
                            }
                            "加载器" | "loader" | "l" => {
                                if crate::pipeline::is_valid_loader(val) {
                                    *ld = Some(val.to_string());
                                } else {
                                    eprintln!("{}", paint(RED, "✗ 无效加载器 '{val}', 可选: fabric / forge / neoforge / quilt"));
                                }
                            }
                            "数量" | "limit" | "n" => match val.parse::<u32>() {
                                Ok(x) if (1..=crate::pipeline::MAX_SEARCH_LIMIT).contains(&x) => {
                                    *n = Some(x)
                                }
                                _ => eprintln!(
                                    "{}",
                                    paint(
                                        RED,
                                        &format!(
                                            "✗ 数量需为 1-{} 的整数 (单次对话上限 {})",
                                            crate::pipeline::MAX_SEARCH_LIMIT,
                                            crate::pipeline::MAX_SEARCH_LIMIT
                                        )
                                    )
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
                let input = match crate::pipeline::preset_prefix(
                    preset_gv.as_deref(),
                    preset_loader.as_deref(),
                    preset_limit,
                ) {
                    Some(p) => format!("{p}{input}"),
                    None => input.to_string(),
                };
                print_busy_hint();
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
                    match crate::storage::history::auto_save(&mut ag, &cfg.data_dir()) {
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
    let db = crate::storage::database::UserDatabase::load(&cfg.db_path());
    println!("{}", paint(DIM, "── 会话结束 ──────────────────────"));
    println!("数据库: {}", db.summary());
    println!("本次会话用量: {}", ag.usage_summary());
    Ok(())
}
