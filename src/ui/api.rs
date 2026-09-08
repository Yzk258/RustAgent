//! Web UI 的 API handlers。
//!
//! 扩展约定: 新增接口时在这里写 handler, 再到 `mod.rs::router` 注册一条路由,
//! 前端在 app.js 的 `API` 对象里加对应调用即可, 三步完成一次功能扩展。

use axum::body::{Body, Bytes};
use axum::extract::{Query, State};
use axum::response::Response;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use std::convert::Infallible;
use tokio::sync::mpsc::unbounded_channel;
use tokio_stream::wrappers::UnboundedReceiverStream;

use super::AppState;
use crate::agent::{new_agent, AgentEvent};
use crate::prelude::*;
use crate::storage::history;

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
    let cfg = state.cfg.read().await.clone();
    let ag = state.agent.lock().await;
    Json(json!({
        "version": env!("CARGO_PKG_VERSION"),
        "model": cfg.llm.model,
        "base_url": cfg.llm.base_url,
        "download_dir": cfg.output.download_dir,
        "usage_summary": ag.usage_summary(),
        "calls": ag.calls,
        "total_tokens": ag.usage.total_tokens,
        "cost": format!("{:.4}", ag.cost()),
        "session_file": ag.session_file,
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
    let cfg = state.cfg.read().await.clone();
    let db = crate::storage::database::UserDatabase::load(&cfg.db_path());
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
    let cfg = state.cfg.read().await.clone();
    let dir = std::path::Path::new(&cfg.output.download_dir);
    let mut packs: Vec<Value> = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|x| x == "mrpack") {
                let meta = entry.metadata().ok();
                let modified = meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .map(|t| {
                        chrono::DateTime::<chrono::Local>::from(t)
                            .format("%Y-%m-%d %H:%M")
                            .to_string()
                    })
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
    Json(json!({ "dir": cfg.output.download_dir, "packs": packs }))
}

/// 在系统文件管理器中打开整合包输出目录 (目录不存在则先创建)。
/// 注意: config 里的 download_dir 通常是相对路径 (如 ./downloads), 而 explorer
/// 等文件管理器不解析相对路径 (会打开默认位置), 因此必须先转成绝对路径。
/// 转换基准是进程工作目录 —— 与组包写入、列表读取用的是同一个基准, 保证打开的就是真正的输出目录。
pub async fn packs_open(State(state): SharedState) -> Json<Value> {
    let cfg = state.cfg.read().await.clone();
    let dir = &cfg.output.download_dir;
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
// "试试这个" 推荐 + 反馈 (不持 agent 锁, 与对话流并行)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RecommendParams {
    pub game_version: String,
    #[serde(default)]
    pub loader: String,
}

/// 根据 MC 版本/加载器与用户口味推荐 5 个新 mod (复用 pipeline::try_this)。
/// 独立于 agent 锁: 对话进行中也可调用, 不阻塞聊天流。
pub async fn recommend(
    State(state): SharedState,
    Query(params): Query<RecommendParams>,
) -> Json<Value> {
    let cfg = state.cfg.read().await.clone();
    let db = crate::storage::database::UserDatabase::load(&cfg.db_path());
    match crate::pipeline::try_this(&state.modrinth, &db, &params.game_version, &params.loader)
        .await
    {
        Ok(ranked) => {
            let mods: Vec<Value> = ranked
                .iter()
                .take(5)
                .map(|s| {
                    json!({
                        "slug": s.hit.slug,
                        "title": s.hit.title,
                        "description": s.hit.description,
                        "downloads": s.hit.downloads,
                        "categories": s.hit.display_categories,
                        "taste_score": s.score,
                    })
                })
                .collect();
            Json(json!({ "ok": true, "recommendations": mods, "db_summary": db.summary() }))
        }
        Err(e) => Json(json!({
            "ok": false,
            "message": format!("{e:#}"),
            "recommendations": []
        })),
    }
}

#[derive(Deserialize)]
pub struct FeedbackRequest {
    pub slug: String,
    pub verdict: String,
}

/// 记录喜欢/不喜欢 (写入用户数据库)。与对话工具 record_feedback 同一写库逻辑,
/// 但不经过 LLM, 不持 agent 锁, 点击即生效。
pub async fn feedback(State(state): SharedState, Json(req): Json<FeedbackRequest>) -> Json<Value> {
    if req.verdict != "like" && req.verdict != "dislike" {
        return Json(json!({ "ok": false, "message": "verdict 必须是 like 或 dislike" }));
    }
    let cfg = state.cfg.read().await.clone();
    let mut db = crate::storage::database::UserDatabase::load(&cfg.db_path());
    let tags = match state.modrinth.project(&req.slug).await {
        Ok(p) => crate::pipeline::taste_tags(&p.categories),
        Err(_) => Vec::new(),
    };
    db.rate(crate::storage::database::FeedbackRecord {
        slug: req.slug.clone(),
        verdict: req.verdict.clone(),
        tags: tags.clone(),
        game_version: String::new(),
        loader: String::new(),
        source: "web".to_string(),
        timestamp: chrono::Local::now().to_rfc3339(),
    });
    match db.save() {
        Ok(()) => Json(json!({
            "ok": true,
            "slug": req.slug,
            "verdict": req.verdict,
            "db_summary": db.summary()
        })),
        Err(e) => Json(json!({ "ok": false, "message": format!("{e:#}") })),
    }
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
    /// 界面预设栏带来的找包数量 (可选), agent 作为 search_mods 的 limit 使用
    #[serde(default)]
    pub search_limit: Option<u32>,
}

/// 把界面预设拼进用户消息: 无效值静默忽略, 不阻塞对话。
/// 前缀生成逻辑与 CLI /set 共用 pipeline::preset_prefix。
fn apply_preset(
    text: String,
    gv: &Option<String>,
    ld: &Option<String>,
    limit: Option<u32>,
) -> String {
    match crate::pipeline::preset_prefix(gv.as_deref(), ld.as_deref(), limit) {
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
    let data_dir = state.cfg.read().await.data_dir();
    let text = apply_preset(req.text, &req.game_version, &req.loader, req.search_limit);

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
                    AgentEvent::Progress {
                        text,
                        current,
                        total,
                    } => {
                        json!({ "type": "progress", "text": text, "current": current, "total": total })
                    }
                    AgentEvent::ReplyDelta { text } => {
                        json!({ "type": "reply_delta", "text": text })
                    }
                    AgentEvent::Reply { text } => json!({ "type": "reply", "text": text }),
                    AgentEvent::LlmUsage {
                        prompt_tokens,
                        completion_tokens,
                        total_tokens,
                    } => json!({
                        "type": "llm_usage",
                        "prompt_tokens": prompt_tokens,
                        "completion_tokens": completion_tokens,
                        "total_tokens": total_tokens
                    }),
                };
                let _ = fwd_tx.send(Ok(Bytes::from(format!("{line}\n"))));
            }
        });

        // 串行执行一轮对话: 持锁期间同一时刻只服务一个请求
        let mut err_msg: Option<String> = None;
        let saved_path;
        let summary;
        {
            let mut ag = agent.lock().await;
            let res = ag.run_turn_with(&text, &ev_tx).await;
            summary = ag.usage_summary();
            if let Err(e) = res {
                err_msg = Some(format!("{e:#}"));
            }
            // 每轮对话默认自动保存 (打断/出错也保留已有内容), 同一会话覆盖写同一文件
            saved_path = history::auto_save(&mut ag, &data_dir).unwrap_or_default();
        }
        // 关闭事件通道, 等转发任务把剩余事件写完
        drop(ev_tx);
        let _ = forwarder.await;

        // 收尾事件: 先报错 (若有), 再发完成标记
        if let Some(m) = err_msg {
            send_line(json!({ "type": "error", "message": m }));
        }
        send_line(json!({ "type": "done", "usage": summary, "saved": saved_path }));
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

/// 开启新会话: 重建一个全新 Agent (复用同一打断标记实例, 使用当前生效配置)。
/// 若有对话进行中, 先请求打断 —— agent 在安全点秒级收尾并自动保存, 再等锁切换,
/// 已产生的内容不会丢。
pub async fn session_new(State(state): SharedState) -> Json<Value> {
    state
        .interrupt
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let cfg = state.cfg.read().await.clone();
    match new_agent(&cfg, state.interrupt.clone()).await {
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
    state
        .interrupt
        .store(true, std::sync::atomic::Ordering::Relaxed);
    Json(json!({ "ok": true, "message": "已发送打断请求, 任务将在安全点停止" }))
}

/// 保存当前会话到 userdata/sessions/
pub async fn session_save(State(state): SharedState) -> Json<Value> {
    let data_dir = state.cfg.read().await.data_dir();
    let ag = state.agent.lock().await;
    match history::save(&ag, &data_dir) {
        Ok(p) => Json(json!({ "ok": true, "message": format!("已保存: {p}") })),
        Err(e) => Json(json!({ "ok": false, "message": format!("{e:#}") })),
    }
}

/// 加载最近一次保存的会话 (响应附带可渲染消息, 前端据此恢复对话显示)。
/// 对话进行中调用: 先请求打断再等锁, 收尾保存完成后切换。
pub async fn session_load(State(state): SharedState) -> Json<Value> {
    state
        .interrupt
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let data_dir = state.cfg.read().await.data_dir();
    let mut ag = state.agent.lock().await;
    match history::load_latest(&mut ag, &data_dir) {
        Ok(p) => Json(json!({
            "ok": true,
            "message": format!("已加载: {p}"),
            "messages": history::renderable_messages(&ag),
        })),
        Err(e) => Json(json!({ "ok": false, "message": format!("{e:#}") })),
    }
}

/// 列出会话目录下的会话文件, valid 为 true 的才可导入
pub async fn sessions(State(state): SharedState) -> Json<Value> {
    let data_dir = state.cfg.read().await.data_dir();
    let list: Vec<Value> = history::list(&data_dir)
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
    Json(json!({
        "dir": history::sessions_dir(&data_dir),
        "sessions": list
    }))
}

#[derive(Deserialize)]
pub struct ImportRequest {
    pub name: String,
}

/// 导入指定会话文件: 校验合法性后灌入 agent, 并返回可渲染消息。
/// 对话进行中调用: 先请求打断再等锁, 收尾保存完成后切换。
pub async fn session_import(
    State(state): SharedState,
    Json(req): Json<ImportRequest>,
) -> Json<Value> {
    if req.name.contains('/') || req.name.contains('\\') || req.name.contains("..") {
        return Json(json!({ "ok": false, "message": "非法文件名" }));
    }
    state
        .interrupt
        .store(true, std::sync::atomic::Ordering::Relaxed);
    let path = format!(
        "{}/{}",
        history::sessions_dir(&state.cfg.read().await.data_dir()),
        req.name
    );
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
pub async fn session_open(State(state): SharedState) -> Json<Value> {
    let dir = history::sessions_dir(&state.cfg.read().await.data_dir());
    if let Err(e) = std::fs::create_dir_all(&dir) {
        return Json(json!({ "ok": false, "message": format!("无法创建目录: {e:#}") }));
    }
    let abs = std::path::absolute(&dir).unwrap_or_default();
    match open_in_file_manager(&abs.to_string_lossy()) {
        Ok(_) => Json(json!({ "ok": true, "message": format!("已打开目录: {}", abs.display()) })),
        Err(e) => Json(json!({ "ok": false, "message": format!("打开目录失败: {e:#}") })),
    }
}

// ---------------------------------------------------------------------------
// 设置接口: 运行时查看 / 修改 LLM 配置 (热更新 + 写回 config.toml)
// ---------------------------------------------------------------------------

/// 当前 LLM 设置。api_key 脱敏: 只回末 4 位 + 是否已设置, 绝不回传完整 key。
pub async fn settings(State(state): SharedState) -> Json<Value> {
    let cfg = state.cfg.read().await;
    let key = cfg.llm.api_key.as_str();
    let masked = if key.len() > 4 {
        format!("***{}", &key[key.len() - 4..])
    } else if !key.is_empty() {
        "***".to_string()
    } else {
        String::new()
    };
    Json(json!({
        "model": cfg.llm.model,
        "base_url": cfg.llm.base_url,
        "api_key_set": !key.is_empty(),
        "api_key_masked": masked,
        "context_length": cfg.llm.context_length,
        "price_input_per_m": cfg.llm.price_input_per_m,
        "price_output_per_m": cfg.llm.price_output_per_m,
        "token_budget": cfg.llm.token_budget,
        "max_tool_iterations": cfg.llm.max_tool_iterations,
        "thinking": cfg.llm.thinking.clone().unwrap_or_default(),
        "curseforge_enabled": cfg.curseforge.enabled,
    }))
}

#[derive(Deserialize, Default)]
pub struct SettingsRequest {
    base_url: Option<String>,
    /// 留空/缺省 = 保持现有 key 不变
    api_key: Option<String>,
    model: Option<String>,
    context_length: Option<u64>,
    price_input_per_m: Option<f64>,
    price_output_per_m: Option<f64>,
    token_budget: Option<u64>,
    max_tool_iterations: Option<u32>,
    /// 空串 = 清除 (不再发送任何思考参数); 其余原样透传
    thinking: Option<String>,
    /// CurseForge 支持开关 (设置窗口切换; 开→热装客户端, 关→热卸)
    curseforge_enabled: Option<bool>,
}

/// 保存设置: 在配置副本上套用改动 -> 校验 -> 热更新当前 agent (会话保留)
/// -> 写回 config.toml -> 更新全局配置。写回失败不回滚运行时配置, 只在消息中提示。
pub async fn settings_update(
    State(state): SharedState,
    Json(req): Json<SettingsRequest>,
) -> Json<Value> {
    // 基础字段空值校验 (空 = 用户没填完整, 提前报错比静默保持旧值更直观)
    for (name, v) in [("模型", &req.model), ("API 地址", &req.base_url)] {
        if v.as_ref().is_some_and(|s| s.trim().is_empty()) {
            return Json(json!({ "ok": false, "message": format!("{name}不能为空") }));
        }
    }
    let mut cfg = state.cfg.read().await.clone();
    if let Some(v) = req.base_url {
        cfg.llm.base_url = v.trim().to_string();
    }
    if let Some(v) = req.api_key.filter(|s| !s.trim().is_empty()) {
        cfg.llm.api_key = v.trim().to_string();
    }
    if let Some(v) = req.model {
        cfg.llm.model = v.trim().to_string();
    }
    if let Some(v) = req.context_length {
        cfg.llm.context_length = v;
    }
    if let Some(v) = req.price_input_per_m {
        cfg.llm.price_input_per_m = v;
    }
    if let Some(v) = req.price_output_per_m {
        cfg.llm.price_output_per_m = v;
    }
    if let Some(v) = req.token_budget {
        cfg.llm.token_budget = v;
    }
    if let Some(v) = req.max_tool_iterations {
        cfg.llm.max_tool_iterations = v;
    }
    if let Some(v) = req.thinking {
        cfg.llm.thinking = match v.trim() {
            "" => None,
            s => Some(s.to_string()),
        };
    }
    if let Err(e) = cfg.validate() {
        return Json(json!({ "ok": false, "message": format!("设置未保存: {e:#}") }));
    }

    // 热更新当前 agent (锁与聊天互斥: 对话进行中会等本轮结束后再生效)
    let llm = cfg.llm.clone();
    {
        let mut ag = state.agent.lock().await;
        if let Err(e) = ag.update_llm(llm.clone()) {
            return Json(json!({ "ok": false, "message": format!("设置未保存: {e:#}") }));
        }
        if let Some(v) = req.curseforge_enabled {
            ag.update_curseforge(v);
        }
    }

    // 写回 config.toml; 失败不影响本次已生效的运行时配置
    let mut message = match crate::config::save_llm(&state.config_path, &cfg.llm) {
        Ok(()) => format!("已保存, 当前模型: {}", llm.model),
        Err(e) => format!("运行时已生效, 但写入 config.toml 失败: {e:#}"),
    };
    if let Some(v) = req.curseforge_enabled {
        message.push_str(if v {
            "; CurseForge 已开启"
        } else {
            "; CurseForge 已关闭"
        });
        if let Err(e) = crate::config::save_curseforge(&state.config_path, v) {
            message.push_str(&format!(" (写回失败: {e:#})"));
        }
    }
    *state.cfg.write().await = cfg;
    Json(json!({ "ok": true, "message": message, "model": llm.model }))
}
