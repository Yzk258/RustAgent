//! Web UI 模块: 本地 HTTP 服务器 + 内嵌静态前端。
//!
//! 设计原则 (便于扩展):
//! - 所有业务接口挂在 `/api/*` 下, 由 `api.rs` 统一实现, 新功能只需加一个 handler + 一条路由;
//! - 前端三件套 (index.html / style.css / app.js) 通过 include_str! 内嵌, 单二进制即可分发;
//! - 聊天接口用 NDJSON 流式返回 Agent 事件, 前端按事件类型渲染, 新事件类型只需前端加一个 case。

pub mod api;

use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;
use std::sync::Arc;

use crate::agent::{new_agent, Agent};
use crate::config::Config;
use crate::prelude::*;
use crate::tools::ToolRegistry;

/// 共享应用状态: Agent 加互斥锁串行化对话, 配置 RwLock 支持运行时热更新
/// (设置窗口改模型等)。interrupt 是协作式打断标记 (打断按钮置位, agent 在安全点检查),
/// 换 agent 时复用同一实例。需要暴露给接口的新资源直接加字段即可。
pub struct AppState {
    pub agent: Arc<tokio::sync::Mutex<Agent>>,
    pub cfg: Arc<tokio::sync::RwLock<Config>>,
    pub interrupt: Arc<AtomicBool>,
    /// Modrinth 客户端: 供"试试这个"推荐与反馈接口独立使用, 不持 agent 锁, 与对话流并行。
    pub modrinth: crate::providers::modrinth::ModrinthClient,
    /// 独立工具注册表: 供 /api/tool/trial 试用接口直接执行工具 (不经过 agent 对话循环)。
    /// 与 agent 内的 ToolRegistry 并列; 无状态, 互不影响。
    pub tools: ToolRegistry,
    /// config.toml 路径 (设置写回用)
    pub config_path: String,
}

/// 内嵌的静态前端文件 (编译期打包进二进制)
const INDEX_HTML: &str = include_str!("static/index.html");
const STYLE_CSS: &str = include_str!("static/style.css");
const APP_JS: &str = include_str!("static/app.js");
/// 第三方 markdown 渲染库 (marked v12, MIT) — 助手回复的富文本渲染
const MARKED_JS: &str = include_str!("static/marked.min.js");

/// 启动 Web UI 服务器 (cargo run -- ui)。config_path 用于设置窗口把改动写回配置文件。
/// auto_open=false 时跳过"自动打开浏览器"(cargo run -- ui --no-browser)。
pub async fn serve(cfg: Config, config_path: &str, auto_open: bool) -> Result<()> {
    let interrupt = Arc::new(AtomicBool::new(false));
    let port = cfg.ui.port;
    let agent = Arc::new(tokio::sync::Mutex::new(
        new_agent(&cfg, interrupt.clone()).await?,
    ));
    let modrinth = crate::providers::modrinth::ModrinthClient::new()?;
    let cf = cfg
        .curseforge
        .enabled
        .then(crate::providers::curseforge::CfClient::new);
    let tools = ToolRegistry::new(
        modrinth.clone(),
        cf,
        &cfg.output.download_dir,
        cfg.db_path(),
    );
    let state = AppState {
        agent,
        cfg: Arc::new(tokio::sync::RwLock::new(cfg)),
        interrupt,
        modrinth,
        tools,
        config_path: config_path.to_string(),
    };

    let app = router(state);

    let addr = format!("127.0.0.1:{port}");
    let url = format!("http://{addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    // 服务器一定先起来再谈"怎么打开界面": 地址单独占一行, 方便双击选中复制
    println!("RustAgent Web UI 已启动: {url}");
    // 两种模式都把地址放进剪贴板: 自动打开失败时这是最省事的手动路径
    if copy_to_clipboard(&url) {
        println!("(地址已复制到剪贴板)");
    }
    let rid = integrity_rid();
    if !auto_open {
        println!("(--no-browser: 请在浏览器地址栏粘贴上面的地址)");
    } else if should_skip_auto_open(rid) {
        // 低 IL 下这条转发必然被 UIPI 拒绝: 浏览器会自己弹一个 Windows 对话框
        // ("未响应 / 现有实例正在以提升的权限运行"), 我们收不到错误码。既然注定失败就别做。
        println!(
            "(本进程完整性级别 {}: 系统会拒绝跨级别转交 URL, 已跳过自动打开浏览器 —— 请在浏览器地址栏粘贴上面的地址)",
            integrity_label(rid)
        );
    } else {
        // 尽力而为: 失败发生在浏览器进程内部, 这里拿不到错误码, 所以只提示用户手动粘贴
        open_browser(&url);
        println!("已尝试自动打开浏览器; 界面没弹出时在浏览器地址栏粘贴上面的地址即可");
    }
    println!("按 Ctrl+C 停止服务器");

    axum::serve(listener, app).await?;
    Ok(())
}

/// 是否自动打开浏览器: 只有显式传 `--no-browser` 才关掉 (`cargo run -- ui --no-browser`)。
/// 单独抽出来是为了能测: 被沙箱降权的环境里自动打开必然失败, 这个开关是唯一的出路。
pub fn auto_open_enabled(extra_args: &[String]) -> bool {
    !extra_args.iter().any(|a| a == "--no-browser")
}

/// 路由注册中心。新增页面或接口时在这里追加一条路由即可。
fn router(state: AppState) -> Router {
    Router::new()
        // 静态页面
        .route(
            "/",
            get(|| async {
                (
                    [
                        (header::CONTENT_TYPE, "text/html; charset=utf-8"),
                        (header::CACHE_CONTROL, "no-cache"),
                    ],
                    INDEX_HTML,
                )
                    .into_response()
            }),
        )
        .route(
            "/style.css",
            get(|| async { css_response(STYLE_CSS).await }),
        )
        .route("/app.js", get(|| async { js_response(APP_JS).await }))
        .route("/marked.min.js", get(|| async { js_response(MARKED_JS).await }))
        // REST API
        .route("/api/health", get(api::health))
        .route("/api/info", get(api::info))
        .route("/api/tools", get(api::tools))
        .route("/api/profile", get(api::profile))
        .route("/api/packs", get(api::packs))
        .route("/api/packs/open", post(api::packs_open))
        // "试试这个"推荐 + 反馈 (不持 agent 锁, 与对话流并行)
        .route("/api/recommend", get(api::recommend))
        .route("/api/feedback", post(api::feedback))
        // 设置 (查看 / 热更新 LLM 配置并写回 config.toml)
        .route(
            "/api/settings",
            get(api::settings).post(api::settings_update),
        )
        // 工具试用 (直接执行工具拿标准输出 + AI 流式分析, 不经过对话循环)
        .route("/api/tool/trial", post(api::tool_trial))
        // 对话主接口: NDJSON 流式返回 Agent 事件
        .route("/api/chat", post(api::chat))
        .route("/api/chat/interrupt", post(api::chat_interrupt))
        // 会话管理
        .route("/api/session/new", post(api::session_new))
        .route("/api/session/import", post(api::session_import))
        .route("/api/session/open", post(api::session_open))
        .route("/api/sessions", get(api::sessions))
        .with_state(Arc::new(state))
}

/// CSS 响应: 正确 Content-Type + 禁缓存, 保证前端更新后浏览器不会用旧文件
async fn css_response(body: &'static str) -> Response {
    (
        [
            (header::CONTENT_TYPE, "text/css; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

/// JS 响应
async fn js_response(body: &'static str) -> Response {
    (
        [
            (
                header::CONTENT_TYPE,
                "application/javascript; charset=utf-8",
            ),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        body,
    )
        .into_response()
}

/// 尝试用系统默认浏览器打开页面。注意这是"尽力而为": `cmd /C start` 自身总会成功返回,
/// 真正的失败 (跨完整性级别的单例转发被 UIPI 拒绝) 发生在浏览器进程内部并弹窗, 这里看不到。
/// 所以低 IL 由 `should_skip_auto_open()` 提前拦掉, 不指望这个函数报错。
fn open_browser(url: &str) {
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}

/// SECURITY_MANDATORY_MEDIUM_RID (Windows 完整性级别里的"中")
const SECURITY_MANDATORY_MEDIUM_RID: u32 = 0x2000;

/// Windows 令牌完整性级别 (MIC) 的最小 FFI —— 只用 kernel32/advapi32 的 6 个函数,
/// 不引第三方依赖 (整个项目对 Win32 的依赖只到 `std::process::Command` 这一层)。
#[cfg(windows)]
mod win_mic {
    use std::ffi::c_void;

    #[link(name = "kernel32")]
    extern "system" {
        fn GetCurrentProcess() -> *mut c_void;
        fn CloseHandle(handle: *mut c_void) -> i32;
    }
    #[link(name = "advapi32")]
    extern "system" {
        fn OpenProcessToken(process: *mut c_void, desired_access: u32, token: *mut *mut c_void) -> i32;
        fn GetTokenInformation(
            token: *mut c_void,
            class: u32,
            info: *mut c_void,
            len: u32,
            ret_len: *mut u32,
        ) -> i32;
        fn GetSidSubAuthorityCount(sid: *mut c_void) -> *mut u8;
        fn GetSidSubAuthority(sid: *mut c_void, index: u32) -> *mut u32;
    }

    const TOKEN_QUERY: u32 = 0x0008;
    const TOKEN_INTEGRITY_LEVEL: u32 = 25;

    /// 当前进程完整性级别 RID: 0=Untrusted, 0x1000=Low, 0x2000=Medium, 0x3000=High。
    /// 读不到返回 None (调用方按"正常"处理, 不改变默认行为)。
    pub fn current_rid() -> Option<u32> {
        unsafe {
            let mut token: *mut c_void = std::ptr::null_mut();
            if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) == 0 {
                return None;
            }
            let mut len: u32 = 0;
            GetTokenInformation(token, TOKEN_INTEGRITY_LEVEL, std::ptr::null_mut(), 0, &mut len);
            if len == 0 {
                CloseHandle(token);
                return None;
            }
            // 用 usize 缓冲保证对齐: TOKEN_MANDATORY_LABEL 的第一个成员就是 SID 指针
            let word = std::mem::size_of::<usize>();
            let words = len as usize / word + 1;
            let mut buf = vec![0usize; words];
            let ok = GetTokenInformation(
                token,
                TOKEN_INTEGRITY_LEVEL,
                buf.as_mut_ptr() as *mut c_void,
                (words * word) as u32,
                &mut len,
            );
            CloseHandle(token);
            if ok == 0 {
                return None;
            }
            let sid = *(buf.as_ptr() as *const *mut c_void);
            if sid.is_null() {
                return None;
            }
            let count = *GetSidSubAuthorityCount(sid) as u32;
            if count == 0 {
                return None;
            }
            Some(*GetSidSubAuthority(sid, count - 1))
        }
    }
}

/// 当前进程的完整性级别 RID (非 Windows 恒为 None)
fn integrity_rid() -> Option<u32> {
    #[cfg(windows)]
    {
        win_mic::current_rid()
    }
    #[cfg(not(windows))]
    {
        None
    }
}

/// RID → 给人看的名字 (只用于启动时那行提示, 让用户一眼看出自己被降权了)
fn integrity_label(rid: Option<u32>) -> &'static str {
    match rid {
        Some(0) => "Untrusted",
        Some(0x1000) => "Low",
        Some(0x2000) => "Medium",
        Some(0x3000) => "High",
        Some(0x4000) => "System",
        _ => "未知",
    }
}

/// 低于 Medium 就跳过"自动打开浏览器": UIPI 会拒绝这种跨完整性级别的单例转发,
/// 唯一结果就是浏览器弹一个"未响应"的 Windows 对话框 (实测: 低 IL 沙箱 + 已运行的 Edge)。
/// 读不到级别时返回 false —— 宁可照常尝试, 也不因为一次探测失败就不给用户开界面。
fn should_skip_auto_open(rid: Option<u32>) -> bool {
    rid.is_some_and(|r| r < SECURITY_MANDATORY_MEDIUM_RID)
}

/// 把地址写进剪贴板 (尽力而为, 失败返回 false)。走系统自带的 clip.exe, 不引入新依赖;
/// 设置剪贴板不同于给窗口发消息, 不受 UIPI 跨完整性级别限制, 所以在低 IL 下通常仍然可用
/// (实测: 低 IL 沙箱内 `cmd /C 'echo <url>| clip'` 可写入)。
fn copy_to_clipboard(text: &str) -> bool {
    #[cfg(windows)]
    {
        // 不让特殊字符进 shell: 这里的 URL 是本地回环地址, 命中即放弃复制 (少一次便利而已)
        if text.chars().any(|c| "|&<>^\"%\r\n".contains(c)) {
            return false;
        }
        return std::process::Command::new("cmd")
            .args(["/C", &format!("echo {text}| clip")])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
    }
    #[cfg(not(windows))]
    {
        let _ = text;
        false
    }
}

#[cfg(test)]
mod tests {
    use super::{auto_open_enabled, integrity_label, integrity_rid, should_skip_auto_open};

    fn args(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    /// 被降权的环境里自动打开必然失败, 所以 `--no-browser` 必须真的能关掉它
    #[test]
    fn no_browser_flag_disables_auto_open() {
        assert!(auto_open_enabled(&args(&[])));
        assert!(auto_open_enabled(&args(&["--verbose"])));
        assert!(!auto_open_enabled(&args(&["--no-browser"])));
        assert!(!auto_open_enabled(&args(&["something", "--no-browser"])));
    }

    /// 低 IL 下自动打开只会换来浏览器的一个报错对话框, 必须自动跳过
    #[test]
    fn auto_open_is_skipped_below_medium_integrity() {
        assert!(should_skip_auto_open(Some(0))); // Untrusted
        assert!(should_skip_auto_open(Some(0x1000))); // Low
        assert!(!should_skip_auto_open(Some(0x2000))); // Medium: 照常打开
        assert!(!should_skip_auto_open(Some(0x3000))); // High
        assert!(!should_skip_auto_open(None)); // 探测失败时不改变默认行为
    }

    #[test]
    fn integrity_labels_are_human_readable() {
        assert_eq!(integrity_label(Some(0)), "Untrusted");
        assert_eq!(integrity_label(Some(0x1000)), "Low");
        assert_eq!(integrity_label(Some(0x2000)), "Medium");
        assert_eq!(integrity_label(Some(0x3000)), "High");
        assert_eq!(integrity_label(None), "未知");
    }

    /// 真读一次本进程: 证明那段 FFI 不会崩, 且拿到的是已知的级别值
    #[test]
    #[cfg(windows)]
    fn current_integrity_level_is_readable() {
        let rid = integrity_rid().expect("Windows 上应当能读到本进程的完整性级别");
        assert!(
            matches!(rid, 0 | 0x1000 | 0x2000 | 0x3000 | 0x4000),
            "意外的完整性级别 RID: {rid:#x}"
        );
    }
}
