//! CLI 视觉样式 (仿 opencode / claude code 风格): 圆角边框横幅、accent 提示符、
//! 彩色状态行。零依赖: 直接输出 ANSI 转义码, Windows 启动时启用 VT 处理。

use crate::config::Config;
use std::io::Write;

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
