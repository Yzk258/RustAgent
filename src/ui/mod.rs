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

/// 共享应用状态: Agent 加互斥锁串行化对话, 配置 RwLock 支持运行时热更新
/// (设置窗口改模型等)。interrupt 是协作式打断标记 (打断按钮置位, agent 在安全点检查),
/// 换 agent 时复用同一实例。需要暴露给接口的新资源直接加字段即可。
pub struct AppState {
    pub agent: Arc<tokio::sync::Mutex<Agent>>,
    pub cfg: Arc<tokio::sync::RwLock<Config>>,
    pub interrupt: Arc<AtomicBool>,
    /// Modrinth 客户端: 供"试试这个"推荐与反馈接口独立使用, 不持 agent 锁, 与对话流并行。
    pub modrinth: crate::providers::modrinth::ModrinthClient,
    /// config.toml 路径 (设置写回用)
    pub config_path: String,
}

/// 内嵌的静态前端文件 (编译期打包进二进制)
const INDEX_HTML: &str = include_str!("static/index.html");
const STYLE_CSS: &str = include_str!("static/style.css");
const APP_JS: &str = include_str!("static/app.js");

/// 启动 Web UI 服务器 (cargo run -- ui)。config_path 用于设置窗口把改动写回配置文件。
pub async fn serve(cfg: Config, config_path: &str) -> Result<()> {
    let interrupt = Arc::new(AtomicBool::new(false));
    let port = cfg.ui.port;
    let agent = Arc::new(tokio::sync::Mutex::new(
        new_agent(&cfg, interrupt.clone()).await?,
    ));
    let modrinth = crate::providers::modrinth::ModrinthClient::new()?;
    let state = AppState {
        agent,
        cfg: Arc::new(tokio::sync::RwLock::new(cfg)),
        interrupt,
        modrinth,
        config_path: config_path.to_string(),
    };

    let app = router(state);

    let addr = format!("127.0.0.1:{port}");
    let url = format!("http://{addr}");
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    println!("RustAgent Web UI 已启动: {url}");
    println!("按 Ctrl+C 停止服务器");
    open_browser(&url);

    axum::serve(listener, app).await?;
    Ok(())
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
        // 聊天 (NDJSON 流式返回 Agent 事件)
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

/// 尝试用系统默认浏览器打开页面, 失败静默忽略 (用户可手动输入地址)
fn open_browser(url: &str) {
    #[cfg(windows)]
    let _ = std::process::Command::new("cmd")
        .args(["/C", "start", "", url])
        .spawn();
    #[cfg(not(windows))]
    let _ = std::process::Command::new("xdg-open").arg(url).spawn();
}
