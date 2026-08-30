use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

use crate::config::LlmConfig;

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Message {
    pub role: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_calls: Option<Vec<ToolCall>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn system(content: &str) -> Self {
        Self { role: "system".into(), content: Some(content.into()), tool_calls: None, tool_call_id: None }
    }

    pub fn user(content: &str) -> Self {
        Self { role: "user".into(), content: Some(content.into()), tool_calls: None, tool_call_id: None }
    }

    pub fn tool(call_id: &str, content: String) -> Self {
        Self { role: "tool".into(), content: Some(content), tool_calls: None, tool_call_id: Some(call_id.into()) }
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct ToolCall {
    pub id: String,
    #[serde(rename = "type")]
    pub call_type: String,
    pub function: FunctionCall,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct FunctionCall {
    pub name: String,
    pub arguments: String,
}

#[derive(Serialize, Clone)]
pub struct ToolDef {
    #[serde(rename = "type")]
    pub def_type: String,
    pub function: FunctionDef,
}

#[derive(Serialize, Clone)]
pub struct FunctionDef {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

impl ToolDef {
    pub fn function(name: &str, description: &str, parameters: serde_json::Value) -> Self {
        Self {
            def_type: "function".into(),
            function: FunctionDef { name: name.into(), description: description.into(), parameters },
        }
    }
}

#[derive(Serialize)]
struct ChatRequest<'a> {
    model: &'a str,
    messages: Vec<Message>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<ToolDef>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_choice: Option<String>,
}

#[derive(Deserialize)]
struct ChatResponse {
    choices: Vec<Choice>,
    usage: Option<Usage>,
}

#[derive(Deserialize)]
struct Choice {
    message: Message,
}

#[derive(Serialize, Deserialize, Clone, Copy, Default, Debug)]
pub struct Usage {
    #[serde(default)]
    pub prompt_tokens: u64,
    #[serde(default)]
    pub completion_tokens: u64,
    #[serde(default)]
    pub total_tokens: u64,
}

pub struct LlmClient {
    http: reqwest::Client,
    cfg: LlmConfig,
}

pub struct ChatResult {
    pub message: Message,
    pub usage: Usage,
}

impl LlmClient {
    pub fn new(cfg: LlmConfig) -> Result<Self> {
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(120))
            .build()?;
        Ok(Self { http, cfg })
    }

    pub async fn chat(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDef>>,
    ) -> Result<ChatResult> {
        let tool_choice = tools.as_ref().map(|_| "auto".to_string());
        let req = ChatRequest {
            model: &self.cfg.model,
            messages,
            tools,
            tool_choice,
        };
        let mut call = self
            .http
            .post(format!("{}/chat/completions", self.cfg.base_url.trim_end_matches('/')))
            .json(&req);
        if !self.cfg.api_key.is_empty() {
            call = call.bearer_auth(&self.cfg.api_key);
        }
        let resp = call.send().await?;
        let status = resp.status();
        if !status.is_success() {
            let body = resp.text().await.unwrap_or_default();
            bail!("LLM API 错误 {status}: {body}");
        }
        let resp: ChatResponse = resp.json().await?;
        let message = resp
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| anyhow::anyhow!("LLM 返回为空"))?
            .message;
        let usage = resp.usage.unwrap_or_default();
        Ok(ChatResult { message, usage })
    }
}
