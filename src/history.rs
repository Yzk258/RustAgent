use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

use crate::agent::Agent;
use crate::llm::Message;

#[derive(Serialize, Deserialize)]
pub struct Session {
    pub messages: Vec<Message>,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub total_tokens: u64,
    pub calls: u64,
    pub saved_at: String,
}

pub const SESSION_DIR: &str = "sessions";

/// 会话文件列表条目, valid 表示能通过 Session 反序列化校验 (合法可导入)
pub struct SessionInfo {
    pub name: String,
    pub size_kb: u64,
    pub modified: String,
    pub valid: bool,
}

pub fn save(agent: &Agent) -> Result<String> {
    std::fs::create_dir_all(SESSION_DIR)?;
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let path = format!("{SESSION_DIR}/session-{ts}.json");
    let session = Session {
        messages: agent.messages.clone(),
        prompt_tokens: agent.usage.prompt_tokens,
        completion_tokens: agent.usage.completion_tokens,
        total_tokens: agent.usage.total_tokens,
        calls: agent.calls,
        saved_at: chrono::Local::now().to_rfc3339(),
    };
    std::fs::write(&path, serde_json::to_string_pretty(&session)?)?;
    Ok(path)
}

pub fn load_latest(agent: &mut Agent) -> Result<String> {
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(SESSION_DIR)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "json"))
        .collect();
    files.sort();
    let path = files
        .pop()
        .ok_or_else(|| anyhow::anyhow!("没有可加载的历史会话"))?;
    load_path(agent, &path.display().to_string())
}

/// 加载指定会话文件。serde 解析即合法性校验: 解析失败时 agent 保持原状
pub fn load_path(agent: &mut Agent, path: &str) -> Result<String> {
    let text = std::fs::read_to_string(path).with_context(|| format!("读取 {path} 失败"))?;
    let session: Session =
        serde_json::from_str(&text).with_context(|| format!("{path} 不是合法的会话文件"))?;
    agent.messages = session.messages;
    agent.usage.prompt_tokens = session.prompt_tokens;
    agent.usage.completion_tokens = session.completion_tokens;
    agent.usage.total_tokens = session.total_tokens;
    agent.calls = session.calls;
    Ok(path.to_string())
}

/// 列出会话目录下的 .json 文件并逐个校验合法性
pub fn list() -> Vec<SessionInfo> {
    let mut out: Vec<SessionInfo> = Vec::new();
    let Ok(entries) = std::fs::read_dir(SESSION_DIR) else {
        return out;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.extension().is_some_and(|e| e == "json") {
            let meta = entry.metadata().ok();
            let valid = std::fs::read_to_string(&path)
                .ok()
                .and_then(|t| serde_json::from_str::<Session>(&t).ok())
                .is_some();
            out.push(SessionInfo {
                name: path
                    .file_name()
                    .map(|s| s.to_string_lossy().to_string())
                    .unwrap_or_default(),
                size_kb: meta.as_ref().map(|m| m.len() / 1024).unwrap_or(0),
                modified: meta
                    .as_ref()
                    .and_then(|m| m.modified().ok())
                    .map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_default(),
                valid,
            });
        }
    }
    out.sort_by(|a, b| b.name.cmp(&a.name));
    out
}

/// 提取可渲染的消息: user 气泡 / assistant 气泡 / 工具调用行, system 与 tool 结果不展示
pub fn renderable_messages(agent: &Agent) -> Vec<serde_json::Value> {
    use serde_json::json;
    let mut out = Vec::new();
    for m in &agent.messages {
        match m.role.as_str() {
            "user" => {
                if let Some(t) = m.content.as_deref().filter(|c| !c.is_empty()) {
                    out.push(json!({ "kind": "user", "text": t }));
                }
            }
            "assistant" => {
                if let Some(t) = m.content.as_deref().filter(|c| !c.is_empty()) {
                    out.push(json!({ "kind": "assistant", "text": t }));
                }
                if let Some(calls) = &m.tool_calls {
                    for c in calls {
                        out.push(json!({
                            "kind": "tool",
                            "text": format!("{}({})", c.function.name, c.function.arguments)
                        }));
                    }
                }
            }
            _ => {}
        }
    }
    out
}
