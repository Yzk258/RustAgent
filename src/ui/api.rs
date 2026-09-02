//! Web UI 的 API handlers。
//!
//! 扩展约定: 新增接口时在这里写 handler, 再到 `mod.rs::router` 注册一条路由,
//! 前端在 app.js 的 `API` 对象里加对应调用即可, 三步完成一次功能扩展。

use axum::body::{Body, Bytes};
use axum::extract::State;
use axum::response::Response;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use tokio::sync::mpsc::unbounded_channel;
use tokio_stream::wrappers::UnboundedReceiverStream;

use super::AppState;
use crate::agent::{new_agent, AgentEvent};
use crate::history;

/// 所有 handler 共享的状态提取器类型
type SharedState = State<std::sync::Arc<AppState>>;

// ---------------------------------------------------------------------------
// 基础信息接口
// ---------------------------------------------------------------------------

/// 健康检查: 前端用它判断服务器是否在线
pub async fn health() -> Json<Value> {
    Json(json!({ "ok": true, "version": env!("CARGO_PKG_VERSION") }))
}

/// 运行信息: 模型、输出目录、当前会话用量 (不含 api_key 等敏感信息)
pub async fn info(State(state): SharedState) -> Json<Value> {
    let ag = state.agent.lock().await;
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "model": state.cfg.llm.model,
        "base_url": state.cfg.llm.base_url,
        "download_dir": state.cfg.output.download_dir,
        "usage_summary": ag.usage_summary(),
        "calls": ag.calls,
        "total_tokens": ag.usage.total_tokens,
        "cost": format!("{:.4}", ag.cost()),
    }))
}

/// 可用工具列表: 直接返回 ToolRegistry 的工具定义, 前端可展示给用户
pub async fn tools() -> Json<Value> {
    let defs = crate::tools::ToolRegistry::defs();
    Json(json!({
        "tools": defs
            .iter()
            .map(|d| json!({ "name": d.function.name, "description": d.function.description }))
            .collect::<Vec<_>>()
    }))
}

/// 用户口味数据库: 反馈统计 + 标签权重 + 历史组包记录
pub async fn profile(State(state): SharedState) -> Json<Value> {
    let db = crate::database::UserDatabase::load(&state.cfg.db_path());
    // 标签权重按绝对值从高到低排序, 只取前 12 个展示
    let mut weights: Vec<(String, f64)> = db.tag_weights().into_iter().collect();
    weights.sort_by(|a, b| {
        b.1.abs()
            .partial_cmp(&a.1.abs())
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    Json(json!({
        "summary": db.summary(),
        "tag_weights": weights
            .iter()
            .take(12)
            .map(|(t, w)| json!({ "tag": t, "weight": w }))
            .collect::<Vec<_>>(),
        "packs": db.packs,
    }))
}

/// 列出输出目录里已生成的 .mrpack 整合包文件
pub async fn packs(State(state): SharedState) -> Json<Value> {
    let dir = std::path::Path::new(&state.cfg.output.download_dir);
    let mut packs: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|x| x == "mrpack") {
                let meta = entry.metadata().ok();
                let modified = meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default();
                packs.push(json!({
                    "name": path.file_name().map(|s| s.to_string_lossy()).unwrap_or_default(),
                    "size_kb": meta.as_ref().map(|m| m.len() / 1024).unwrap_or(0),
                    "modified": modified,
                }));
            }
        }
    }
    // 按修改时间倒序, 最新的排前面
    packs.sort_by(|a, b| b["modified"].as_str().cmp(&a["modified"].as_str()));
    Json(json!({ "dir": state.cfg.output.download_dir, "packs": packs }))
}

/// 在系统文件管理器中打开整合包输出目录 (目录不存在则先创建)。
/// 注意: config 里的 download_dir 通常是相对路径 (如 ./downloads), 而 explorer
/// 等文件管理器不解析相对路径 (会打开默认位置), 因此必须先转成绝对路径。
/// 转换基准是进程工作目录 —— 与组包写入、列表读取用的是同一个基准, 保证打开的就是真正的输出目录。
pub async fn packs_open(State(state): SharedState) -> Json<Value> {
    let dir = &state.cfg.output.download_dir;
    if let Err(e) = std::fs::create_dir_all(dir) {
        return Json(json!({ "ok": false, "message": format!("无法创建目录 {dir}: {e:#}") }));
    }
    let abs = match std::path::absolute(dir) {
        Ok(p) => p,
        Err(e) => return Json(json!({ "ok": false, "message": format!("路径解析失败: {e:#}") })),
    };
    match open_in_file_manager(&abs.to_string_lossy()) {
        Ok(_) => Json(json!({ "ok": true, "message": format!("已打开目录: {}", abs.display()) })),
        Err(e) => Json(json!({ "ok": false, "message": format!("打开目录失败: {e:#}") })),
    }
}

fn open_in_file_manager(path: &str) -> anyhow::Result<()> {
    #[cfg(windows)]
    std::process::Command::new("explorer").arg(path).spawn()?;
    #[cfg(target_os = "macos")]
    std::process::Command::new("open").arg(path).spawn()?;
    #[cfg(all(unix, not(target_os = "macos")))]
    std::process::Command::new("xdg-open").arg(path).spawn()?;
    Ok(())
}

// ---------------------------------------------------------------------------
// 聊天接口: NDJSON 流式返回 Agent 事件
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ChatRequest {
    pub text: String,
    /// 界面预设栏带来的 MC 版本 (可选), 校验通过则注入消息, agent 不再追问
    #[serde(default)]
    pub game_version: Option<String>,
    /// 界面预设栏带来的加载器 (可选)
    #[serde(default)]
    pub loader: Option<String>,
    /// 界面预设栏带来的找包数量 (可选), agent 作为 search_mods 的 limit 使用。
    /// 宽容类型: 数字或数字字符串都接受, 非法值忽略, 避免反序列化失败引发 422。
    #[serde(default)]
    pub search_limit: Option<serde_json::Value>,
}

/// 解析找包数量: 接受 JSON 数字或数字字符串, 单次对话上限 20, 其余返回 None
fn parse_limit(v: &Option<serde_json::Value>) -> Option<u32> {
    let n = match v.as_ref()? {
        serde_json::Value::Number(n) => n.as_u64()?,
        serde_json::Value::String(s) => s.trim().parse::<u64>().ok()?,
        _ => return None,
    };
    u32::try_from(n).ok().filter(|n| (1..=20).contains(n))
}

/// 把界面预设拼进用户消息: 无效值静默忽略, 不阻塞对话。
/// 前缀生成逻辑与 CLI /preset 共用 pipeline::preset_prefix。
fn apply_preset(
    text: String,
    gv: &Option<String>,
    ld: &Option<String>,
    limit: &Option<serde_json::Value>,
) -> String {
    match crate::pipeline::preset_prefix(gv.as_deref(), ld.as_deref(), parse_limit(limit)) {
        Some(p) => format!("{p}{text}"),
        None => text,
    }
}

/// 聊天主接口。请求体 { "text": "..." }, 响应为逐行 JSON (NDJSON):
///   {"type":"tool_call","name":"...","args":"..."}  工具开始调用
///   {"type":"tool_result","name":"...","ok":true}   工具执行完毕
///   {"type":"reply","text":"..."}                    最终回复
///   {"type":"error","message":"..."}                 本轮出错
///   {"type":"done","usage":"..."}                    本轮结束 (附用量摘要)
/// 前端逐行解析并按 type 渲染; 未来新增事件类型只需前后端各加一个分支。
pub async fn chat(State(state): SharedState, Json(req): Json<ChatRequest>) -> Response {
    let (out_tx, out_rx) = unbounded_channel::<Result<Bytes, Infallible>>();
    let agent = state.agent.clone();
    let text = apply_preset(req.text, &req.game_version, &req.loader, &req.search_limit);

    tokio::spawn(async move {
        // 行发送闭包: 把一条 JSON 值作为一行 NDJSON 写入响应流
        let send_line = |v: Value| {
            let _ = out_tx.send(Ok(Bytes::from(format!("{v}\n"))));
        };

        // Agent 事件通道: agent 内部事件 -> NDJSON 行
        let (ev_tx, mut ev_rx) = unbounded_channel::<AgentEvent>();
        let fwd_tx = out_tx.clone();
        let forwarder = tokio::spawn(async move {
            while let Some(ev) = ev_rx.recv().await {
                let line = match ev {
                    AgentEvent::ToolCall { name, args } => {
                        json!({ "type": "tool_call", "name": name, "args": args })
                    }
                    AgentEvent::ToolResult { name, ok } => {
                        json!({ "type": "tool_result", "name": name, "ok": ok })
                    }
                    AgentEvent::Progress { text } => json!({ "type": "progress", "text": text }),
                    AgentEvent::Reply { text } => json!({ "type": "reply", "text": text }),
                };
                let _ = fwd_tx.send(Ok(Bytes::from(format!("{line}\n"))));
            }
        });

        // 串行执行一轮对话: 持锁期间同一时刻只服务一个请求
        let mut err_msg: Option<String> = None;
        let summary;
        {
            let mut ag = agent.lock().await;
            let res = ag.run_turn_with(&text, &ev_tx).await;
            summary = ag.usage_summary();
            if let Err(e) = res {
                err_msg = Some(format!("{e:#}"));
            }
        }
        // 关闭事件通道, 等转发任务把剩余事件写完
        drop(ev_tx);
        let _ = forwarder.await;

        // 收尾事件: 先报错 (若有), 再发完成标记
        if let Some(m) = err_msg {
            send_line(json!({ "type": "error", "message": m }));
        }
        send_line(json!({ "type": "done", "usage": summary }));
    });

    // 把通道包装成字节流返回, 客户端边收边渲染
    let stream = UnboundedReceiverStream::new(out_rx);
    Response::builder()
        .header("content-type", "application/x-ndjson; charset=utf-8")
        .header("cache-control", "no-cache")
        .body(Body::from_stream(stream))
        .unwrap()
}

// ---------------------------------------------------------------------------
// 会话管理接口 (对应 CLI 的 /new /save /load)
// ---------------------------------------------------------------------------

/// 开启新会话: 重建一个全新 Agent (复用同一打断标记实例)
pub async fn session_new(State(state): SharedState) -> Json<Value> {
    match new_agent(
        &state.cfg.llm,
        &state.cfg.output.download_dir,
        &state.cfg.db_path(),
        state.interrupt.clone(),
    )
    .await
    {
        Ok(a) => {
            *state.agent.lock().await = a;
            Json(json!({ "ok": true, "message": "已开启新会话" }))
        }
        Err(e) => Json(json!({ "ok": false, "message": format!("{e:#}") })),
    }
}

/// 打断当前对话轮: 置位协作式取消标记, agent 在下一个安全点收尾
/// (LLM 调用即时中止; 工具间隙逐个停, 不需 agent 互斥锁)
pub async fn chat_interrupt(State(state): SharedState) -> Json<Value> {
    state.interrupt.store(true, std::sync::atomic::Ordering::Relaxed);
    Json(json!({ "ok": true, "message": "已发送打断请求, 任务将在安全点停止" }))
}

/// 保存当前会话到 sessions/
pub async fn session_save(State(state): SharedState) -> Json<Value> {
    let ag = state.agent.lock().await;
    match history::save(&ag) {
        Ok(p) => Json(json!({ "ok": true, "message": format!("已保存: {p}") })),
        Err(e) => Json(json!({ "ok": false, "message": format!("{e:#}") })),
    }
}

/// 加载最近一次保存的会话 (响应附带可渲染消息, 前端据此恢复对话显示)
pub async fn session_load(State(state): SharedState) -> Json<Value> {
    let mut ag = state.agent.lock().await;
    match history::load_latest(&mut ag) {
        Ok(p) => Json(json!({
            "ok": true,
            "message": format!("已加载: {p}"),
            "messages": history::renderable_messages(&ag),
        })),
        Err(e) => Json(json!({ "ok": false, "message": format!("{e:#}") })),
    }
}

/// 列出 sessions/ 下的会话文件, valid 为 true 的才可导入
pub async fn sessions() -> Json<Value> {
    let list: Vec<Value> = history::list()
        .into_iter()
        .map(|s| {
            json!({
                "name": s.name,
                "size_kb": s.size_kb,
                "modified": s.modified,
                "valid": s.valid,
            })
        })
        .collect();
    Json(json!({ "dir": history::SESSION_DIR, "sessions": list }))
}

#[derive(Deserialize)]
pub struct ImportRequest {
    pub name: String,
}

/// 导入指定会话文件: 校验合法性后灌入 agent, 并返回可渲染消息
pub async fn session_import(State(state): SharedState, Json(req): Json<ImportRequest>) -> Json<Value> {
    if req.name.contains('/') || req.name.contains('\\') || req.name.contains("..") {
        return Json(json!({ "ok": false, "message": "非法文件名" }));
    }
    let path = format!("{}/{}", history::SESSION_DIR, req.name);
    let mut ag = state.agent.lock().await;
    match history::load_path(&mut ag, &path) {
        Ok(p) => Json(json!({
            "ok": true,
            "message": format!("已导入: {p}"),
            "messages": history::renderable_messages(&ag),
        })),
        Err(e) => Json(json!({ "ok": false, "message": format!("{e:#}") })),
    }
}

/// 在系统文件管理器中打开会话目录 (用户可手动放入合法的 session json 实现导入)
pub async fn session_open() -> Json<Value> {
    if let Err(e) = std::fs::create_dir_all(history::SESSION_DIR) {
        return Json(json!({ "ok": false, "message": format!("无法创建目录: {e:#}") }));
    }
    let abs = std::path::absolute(history::SESSION_DIR).unwrap_or_default();
    match open_in_file_manager(&abs.to_string_lossy()) {
        Ok(_) => Json(json!({ "ok": true, "message": format!("已打开目录: {}", abs.display()) })),
        Err(e) => Json(json!({ "ok": false, "message": format!("打开目录失败: {e:#}") })),
    }
}
