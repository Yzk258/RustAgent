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
    println!("{} {}{} {}", paint(ACCENT, "│"), paint(BOLD, &title), pad, paint(ACCENT, "│"));
    let pad = " ".repeat(inner - display_width(&sub));
    println!("{} {}{} {}", paint(ACCENT, "│"), paint(DIM, &sub), pad, paint(ACCENT, "│"));
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
            "  Ctrl+C 打断当前任务 (不退出) · 示例: 我想要 1.21.1 fabric 的生存整合包"
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
