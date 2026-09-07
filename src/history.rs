use crate::prelude::*;

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

/// 会话目录: 用户数据根目录 (config.data_dir, 即 db 文件所在目录) 下的 sessions/
pub fn sessions_dir(data_dir: &str) -> String {
    format!("{data_dir}/sessions")
}

/// 会话文件列表条目, valid 表示能通过 Session 反序列化校验 (合法可导入)
pub struct SessionInfo {
    pub name: String,
    pub size_kb: u64,
    pub modified: String,
    pub valid: bool,
}

fn write_to(agent: &Agent, path: &str) -> Result<()> {
    let session = Session {
        messages: agent.messages.clone(),
        prompt_tokens: agent.usage.prompt_tokens,
        completion_tokens: agent.usage.completion_tokens,
        total_tokens: agent.usage.total_tokens,
        calls: agent.calls,
        saved_at: chrono::Local::now().to_rfc3339(),
    };
    let target = std::path::Path::new(path);
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let text = serde_json::to_string_pretty(&session)?;
    let temp = target.with_extension(format!("tmp-{}", std::process::id()));
    std::fs::write(&temp, text)?;
    if let Err(err) = std::fs::rename(&temp, target) {
        if target.exists() {
            std::fs::remove_file(target)?;
            std::fs::rename(&temp, target)?;
        } else {
            return Err(err.into());
        }
    }
    Ok(())
}

/// 手动另存: 每次生成一个带时间戳的新快照文件
pub fn save(agent: &Agent, data_dir: &str) -> Result<String> {
    let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
    let path = format!("{}/session-{ts}.json", sessions_dir(data_dir));
    write_to(agent, &path)?;
    Ok(path)
}

/// 每轮对话结束后自动保存: 同一会话固定写同一个 auto-{ts}.json (ts = 首轮时间),
/// 文件随对话推进持续覆盖更新; 尚无用户消息时跳过。返回空串表示无可保存内容。
pub fn auto_save(agent: &mut Agent, data_dir: &str) -> Result<String> {
    if !agent.messages.iter().any(|m| m.role == "user") {
        return Ok(String::new());
    }
    if agent.session_file.is_none() {
        let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
        agent.session_file = Some(format!("{}/auto-{ts}.json", sessions_dir(data_dir)));
    }
    let path = agent
        .session_file
        .clone()
        .expect("session_file 已在上一步赋值");
    write_to(agent, &path)?;
    Ok(path)
}

pub fn load_latest(agent: &mut Agent, data_dir: &str) -> Result<String> {
    let mut files: Vec<std::path::PathBuf> = std::fs::read_dir(sessions_dir(data_dir))?
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
    // 重置自动保存目标: 后续对话写入新的 auto-*.json, 不覆盖被加载的历史文件
    agent.session_file = None;
    Ok(path.to_string())
}

/// 列出会话目录下的 .json 文件并逐个校验合法性
pub fn list(data_dir: &str) -> Vec<SessionInfo> {
    let mut out: Vec<SessionInfo> = Vec::new();
    let Ok(entries) = std::fs::read_dir(sessions_dir(data_dir)) else {
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
                    .map(|t| {
                        chrono::DateTime::<chrono::Local>::from(t)
                            .format("%Y-%m-%d %H:%M")
                            .to_string()
                    })
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
