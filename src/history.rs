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

const SESSION_DIR: &str = "sessions";

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
    let text = std::fs::read_to_string(&path).with_context(|| format!("读取 {} 失败", path.display()))?;
    let session: Session = serde_json::from_str(&text)?;
    agent.messages = session.messages;
    agent.usage.prompt_tokens = session.prompt_tokens;
    agent.usage.completion_tokens = session.completion_tokens;
    agent.usage.total_tokens = session.total_tokens;
    agent.calls = session.calls;
    Ok(path.display().to_string())
}
