use crate::prelude::*;

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
        Self {
            role: "system".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn user(content: &str) -> Self {
        Self {
            role: "user".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn assistant(content: &str) -> Self {
        Self {
            role: "assistant".into(),
            content: Some(content.into()),
            tool_calls: None,
            tool_call_id: None,
        }
    }

    pub fn tool(call_id: &str, content: String) -> Self {
        Self {
            role: "tool".into(),
            content: Some(content),
            tool_calls: None,
            tool_call_id: Some(call_id.into()),
        }
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
            function: FunctionDef {
                name: name.into(),
                description: description.into(),
                parameters,
            },
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
    /// 流式调用专用 (chat_stream 设 true, 非流式请求不带该字段)
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<StreamOptions>,
    /// config.thinking 透传的扩展字段 (enable_thinking / reasoning_effort 等)
    #[serde(flatten)]
    extra: serde_json::Map<String, serde_json::Value>,
}

/// 把 thinking 配置解析为请求体扩展字段: auto/空段跳过; k=v 的值尽力转
/// JSON (布尔/数字/对象), 失败则按字符串。provider 各家参数名不同, 故原样透传。
fn parse_thinking(spec: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    for seg in split_thinking_entries(spec) {
        let seg = seg.trim();
        if seg.is_empty() || seg.eq_ignore_ascii_case("auto") {
            continue;
        }
        if let Some((k, v)) = seg.split_once('=') {
            let (k, v) = (k.trim(), v.trim());
            if k.is_empty() {
                continue;
            }
            let val = serde_json::from_str(v)
                .unwrap_or_else(|_| serde_json::Value::String(v.to_string()));
            map.insert(k.to_string(), val);
        }
    }
    map
}

/// 按顶层逗号切分, 跳过引号内与 {}/[] 嵌套中的逗号 (JSON 对象值里会有逗号)
fn split_thinking_entries(spec: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let (mut depth, mut in_str, mut esc) = (0usize, false, false);
    for ch in spec.chars() {
        if esc {
            esc = false;
            cur.push(ch);
            continue;
        }
        match ch {
            '\\' if in_str => {
                esc = true;
                cur.push(ch);
            }
            '"' => {
                in_str = !in_str;
                cur.push(ch);
            }
            '{' | '[' if !in_str => {
                depth += 1;
                cur.push(ch);
            }
            '}' | ']' if !in_str => {
                depth = depth.saturating_sub(1);
                cur.push(ch);
            }
            ',' if !in_str && depth == 0 => out.push(std::mem::take(&mut cur)),
            _ => cur.push(ch),
        }
    }
    out.push(cur);
    out
}

#[derive(Serialize)]
struct StreamOptions {
    include_usage: bool,
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

    /// 流式调用: assistant 文本增量通过 on_delta 逐段回调 (首个 token 即可见),
    /// 完整消息与工具调用在流结束后拼装返回。服务端不支持流式时回退一次性解析。
    pub async fn chat_stream(
        &self,
        messages: Vec<Message>,
        tools: Option<Vec<ToolDef>>,
        mut on_delta: impl FnMut(&str),
    ) -> Result<ChatResult> {
        let tool_choice = tools.as_ref().map(|_| "auto".to_string());
        let extra = self
            .cfg
            .thinking
            .as_deref()
            .map(parse_thinking)
            .unwrap_or_default();
        let req = ChatRequest {
            model: &self.cfg.model,
            messages,
            tools,
            tool_choice,
            stream: Some(true),
            stream_options: Some(StreamOptions {
                include_usage: true,
            }),
            extra,
        };
        let mut call = self
            .http
            .post(format!(
                "{}/chat/completions",
                self.cfg.base_url.trim_end_matches('/')
            ))
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
        // 服务端忽略 stream 参数时返回普通 JSON, 按内容类型回退
        let is_sse = resp
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|v| v.contains("text/event-stream"));
        if !is_sse {
            let resp: ChatResponse = resp.json().await?;
            let message = resp
                .choices
                .into_iter()
                .next()
                .ok_or_else(|| anyhow::anyhow!("LLM 返回为空"))?
                .message;
            if let Some(t) = message.content.as_deref() {
                if !t.is_empty() {
                    on_delta(t);
                }
            }
            return Ok(ChatResult {
                message,
                usage: resp.usage.unwrap_or_default(),
            });
        }

        // SSE 逐行解析: "data: {json}" / "data: [DONE]"
        use tokio_stream::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        let mut content = String::new();
        let mut usage = Usage::default();
        // tool_calls 分片拼装: 同一 index 的 id/name 只出现一次, arguments 逐段追加
        let mut tc_ids: Vec<(usize, String)> = Vec::new();
        let mut tc_names: Vec<(usize, String)> = Vec::new();
        let mut tc_args: Vec<(usize, String)> = Vec::new();
        while let Some(chunk) = stream.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk?));
            while let Some(pos) = buf.find('\n') {
                let line: String = buf.drain(..=pos).collect();
                let line = line.trim();
                let Some(data) = line.strip_prefix("data:") else {
                    continue;
                };
                let data = data.trim();
                if data.is_empty() || data == "[DONE]" {
                    continue;
                }
                let Ok(v) = serde_json::from_str::<serde_json::Value>(data) else {
                    continue;
                };
                if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
                    usage = serde_json::from_value(u.clone()).unwrap_or_default();
                }
                let Some(delta) = v.pointer("/choices/0/delta") else {
                    continue;
                };
                if let Some(c) = delta.get("content").and_then(|c| c.as_str()) {
                    if !c.is_empty() {
                        content.push_str(c);
                        on_delta(c);
                    }
                }
                if let Some(tcs) = delta.get("tool_calls").and_then(|t| t.as_array()) {
                    for tc in tcs {
                        let idx = tc.get("index").and_then(|i| i.as_u64()).unwrap_or(0) as usize;
                        let get_str = |k1: &str, k2: Option<&str>| -> Option<String> {
                            match k2 {
                                Some(k2) => tc
                                    .pointer(&format!("/{k1}/{k2}"))
                                    .and_then(|x| x.as_str())
                                    .map(str::to_string),
                                None => tc.get(k1).and_then(|x| x.as_str()).map(str::to_string),
                            }
                        };
                        if let Some(id) = get_str("id", None) {
                            upsert(&mut tc_ids, idx, id);
                        }
                        if let Some(name) = get_str("function", Some("name")) {
                            upsert(&mut tc_names, idx, name);
                        }
                        if let Some(args) = get_str("function", Some("arguments")) {
                            match tc_args.iter_mut().find(|(i, _)| *i == idx) {
                                Some((_, s)) => s.push_str(&args),
                                None => tc_args.push((idx, args)),
                            }
                        }
                    }
                }
            }
        }
        let tool_calls = if tc_names.is_empty() {
            None
        } else {
            Some(
                tc_names
                    .into_iter()
                    .map(|(i, name)| ToolCall {
                        id: tc_ids
                            .iter()
                            .find(|(j, _)| *j == i)
                            .map(|(_, v)| v.clone())
                            .unwrap_or_default(),
                        call_type: "function".into(),
                        function: FunctionCall {
                            name,
                            arguments: tc_args
                                .iter()
                                .find(|(j, _)| *j == i)
                                .map(|(_, v)| v.clone())
                                .unwrap_or_default(),
                        },
                    })
                    .collect(),
            )
        };
        let message = Message {
            role: "assistant".into(),
            content: if content.is_empty() && tool_calls.is_some() {
                None
            } else {
                Some(content)
            },
            tool_calls,
            tool_call_id: None,
        };
        Ok(ChatResult { message, usage })
    }
}

fn upsert(list: &mut Vec<(usize, String)>, idx: usize, val: String) {
    match list.iter_mut().find(|(i, _)| *i == idx) {
        Some((_, s)) => *s = val,
        None => list.push((idx, val)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_call_fragments_are_combined_by_index() {
        let mut parts = Vec::new();
        upsert(&mut parts, 1, "second".into());
        upsert(&mut parts, 1, "updated".into());
        upsert(&mut parts, 0, "first".into());
        assert_eq!(parts, vec![(1, "updated".into()), (0, "first".into())]);
    }

    #[test]
    fn thinking_spec_parses_into_request_extras() {
        let m = parse_thinking("enable_thinking=false, reasoning_effort=medium");
        assert_eq!(m.get("enable_thinking"), Some(&serde_json::json!(false)));
        assert_eq!(
            m.get("reasoning_effort"),
            Some(&serde_json::json!("medium"))
        );

        let obj = parse_thinking(r#"thinking={"type":"enabled","budget_tokens":4096}"#);
        assert_eq!(
            obj.get("thinking").and_then(|v| v.get("budget_tokens")),
            Some(&serde_json::json!(4096))
        );

        assert!(parse_thinking("auto").is_empty());
        assert!(parse_thinking("").is_empty());
        assert_eq!(parse_thinking("  , broken , x=1").len(), 1);
    }
}
