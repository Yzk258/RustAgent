use anyhow::{bail, Result};
use serde::Serialize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::config::LlmConfig;
use crate::llm::{LlmClient, Message, Usage};
use crate::tools::ToolRegistry;

/// Agent 一轮对话过程中对外发出的事件流。
/// CLI 与 UI 都通过消费这些事件来展示进度, 便于未来接入新的前端 (PCL2 卡片等)。
#[derive(Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentEvent {
    /// 开始调用某个工具 (args 为原始 JSON 参数字符串)
    ToolCall { name: String, args: String },
    /// 某个工具执行完毕 (ok 表示是否成功)
    ToolResult { name: String, ok: bool },
    /// 工具内部的阶段性进展 (如 "正在收集 mod sodium (2/8)"), 原地更新展示
    Progress { text: String },
    /// 最终自然语言回复
    Reply { text: String },
}

const SYSTEM_PROMPT: &str = "你是 Minecraft 模组管理助手 RustAgent。核心原则: 你负责理解与沟通, 正确性由工具保证 —— 绝不凭记忆推荐 mod, 一切 mod 数据必须来自工具返回的真实 API 数据。

工作流程:
1. 理解需求: 确认 Minecraft 版本、加载器(fabric/forge/neoforge)和游玩偏好。用户没说清楚的先问。用户用中文描述主题没关系, 搜索工具会自动转换关键词。特别注意: 若用户消息开头带 [界面预设: ...], 说明版本/加载器/候选数量已在界面选好, 视为用户确认, 直接采用, 绝不要再追问这些信息; 预设中的候选数量应作为 search_mods 的 limit 参数 (单次对话上限 20)。
2. 推荐前先调用 get_user_profile 了解用户口味, 再调用 search_mods 搜索(必须传 game_version 和 loader)。
3. 把候选 mod 以列表呈现: 名称、一句话推荐理由(结合用户口味)、下载量。先不下载, 请用户挑选, 不要替用户做决定。
4. 用户确认后调用 build_modpack 生成整合包(自动补全前置依赖并检测冲突), 报告输出路径与冲突详情。生成的 .mrpack 可拖入 PCL2 等启动器直接安装。限制说明(用户触及时主动解释): 单次对话找包/挑选上限 20 个; 单包用户所选 mod 上限 100 个(前置依赖自动补全与报错修复补入不计入) —— 为考虑轻量化, 敬请谅解, 可建议用户分多轮组包。
5. 用户表达喜欢/不喜欢时调用 record_feedback 记录; 用户想看点新的时调用 recommend_new_mods。
6. 搜索无结果时换个关键词重试, 而不是放弃。
7. 用户贴出启动器报错(如缺少某依赖、mod 不兼容)时: 从报错中提取缺失 mod 的名称, 用 search_mods 找到 slug, 调用 repair_pack 把它补进原整合包, 并告知用户重新拖入启动器安装。
始终用中文回复。同一轮内工具调用失败要向用户说明原因并给出替代方案。";

pub struct Agent {
    llm: LlmClient,
    tools: ToolRegistry,
    llm_cfg: LlmConfig,
    /// 协作式打断标记: UI 的打断按钮置位, agent 在安全点检查并干净收尾
    interrupt: Arc<AtomicBool>,
    pub messages: Vec<Message>,
    pub usage: Usage,
    pub calls: u64,
}

/// 组装一个全新的 Agent (CLI 与 Web UI 共用的构造入口)。
/// 新增底层客户端时在这里统一接线。interrupt 由调用方持有 (UI 换 agent 时复用同一标记)。
pub async fn new_agent(
    cfg: &LlmConfig,
    download_dir: &str,
    db_path: &str,
    interrupt: Arc<AtomicBool>,
) -> Result<Agent> {
    let llm = LlmClient::new(cfg.clone())?;
    let modrinth = crate::modrinth::ModrinthClient::new()?;
    let registry = ToolRegistry::new(modrinth, download_dir, db_path);
    Ok(Agent::new(llm, registry, cfg.clone(), interrupt))
}

impl Agent {
    pub fn new(llm: LlmClient, tools: ToolRegistry, llm_cfg: LlmConfig, interrupt: Arc<AtomicBool>) -> Self {
        Self {
            llm,
            tools,
            llm_cfg,
            interrupt,
            messages: vec![Message::system(SYSTEM_PROMPT)],
            usage: Usage::default(),
            calls: 0,
        }
    }

    fn interrupted(&self) -> bool {
        self.interrupt.load(Ordering::Relaxed)
    }

    /// CLI 入口: 与旧版行为一致, 在终端打印工具调用与最终回复。
    /// 内部复用 run_turn_with, 通过通道接收事件再打印, 保证两端行为同步。
    pub async fn run_turn(&mut self, input: &str) -> Result<()> {
        // 创建事件通道, 并启动一个打印任务消费事件
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let printer = tokio::spawn(async move {
            use crate::cli::{paint, ACCENT, DIM, GREEN, RED, YELLOW};
            while let Some(ev) = rx.recv().await {
                match ev {
                    AgentEvent::ToolCall { name, args } => {
                        println!(
                            "  {} {}({})",
                            paint(DIM, "⚙"),
                            paint(ACCENT, &name),
                            truncate(&args, 70)
                        );
                    }
                    AgentEvent::ToolResult { name, ok } => {
                        if ok {
                            println!("  {} {}", paint(GREEN, "✓"), paint(DIM, &name));
                        } else {
                            println!("  {} {}", paint(RED, "✗"), name);
                        }
                    }
                    AgentEvent::Progress { text } => println!("  {} {text}", paint(YELLOW, "⏳")),
                    AgentEvent::Reply { text } => println!("\n{text}\n"),
                }
            }
        });
        let result = self.run_turn_with(input, &tx).await;
        drop(tx); // 关闭通道, 让打印任务自然结束
        let _ = printer.await;
        result
    }

    /// 通用一轮对话: 事件通过 tx 发出, 调用方决定如何展示。
    /// UI / 未来其他前端直接调用本方法并消费事件, 不再依赖终端打印。
    /// 支持协作式打断: interrupt 标记置位后在安全点收尾 (消息序列保持合法)。
    pub async fn run_turn_with(
        &mut self,
        input: &str,
        tx: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
    ) -> Result<()> {
        self.interrupt.store(false, Ordering::Relaxed); // 清掉上一轮遗留的打断请求
        self.messages.push(Message::user(input));
        let max_iters = self.llm_cfg.max_tool_iterations.max(1) as usize;

        for _ in 0..max_iters {
            self.check_budget()?;
            self.trim_context();

            // LLM 调用可被即时打断 (200ms 轮询标记, reqwest 请求随 future 取消而中止)
            let result = tokio::select! {
                r = self.llm.chat(self.messages.clone(), Some(crate::tools::ToolRegistry::defs())) => r?,
                _ = wait_interrupt(&self.interrupt) => {
                    return self.abort_turn(tx, 0).await;
                }
            };
            self.accumulate(&result.usage);
            let msg = result.message;

            match msg.tool_calls.clone() {
                Some(calls) if !calls.is_empty() => {
                    self.messages.push(msg);
                    for (i, call) in calls.iter().enumerate() {
                        // 工具间隙检查打断: 未执行的调用补占位结果, 保证 tool_calls 都有对应 tool 消息
                        if self.interrupted() {
                            return self.abort_turn(tx, i + 1).await;
                        }
                        let name = &call.function.name;
                        // 发出工具调用事件
                        let _ = tx.send(AgentEvent::ToolCall {
                            name: name.clone(),
                            args: call.function.arguments.clone(),
                        });
                        // 进展通道: 工具内部发 String, 这里转成 Progress 事件转发给前端
                        let (ptx, mut prx) = tokio::sync::mpsc::unbounded_channel::<String>();
                        let fwd = tx.clone();
                        let forwarder = tokio::spawn(async move {
                            while let Some(text) = prx.recv().await {
                                let _ = fwd.send(AgentEvent::Progress { text });
                            }
                        });
                        let (result, ok) = match self
                            .tools
                            .execute(name, &call.function.arguments, Some(&ptx))
                            .await
                        {
                            Ok(v) => (v.to_string(), true),
                            Err(e) => (format!("{{ \"error\": \"{}\" }}", format!("{e:#}").replace('"', "'")), false),
                        };
                        drop(ptx); // 关闭进展通道, 等转发任务排空
                        let _ = forwarder.await;
                        // 发出工具结果事件
                        let _ = tx.send(AgentEvent::ToolResult { name: name.clone(), ok });
                        self.messages.push(Message::tool(&call.id, result));
                    }
                }
                _ => {
                    let text = msg.content.clone().unwrap_or_default();
                    self.messages.push(msg);
                    // 发出最终回复事件
                    let _ = tx.send(AgentEvent::Reply { text });
                    return Ok(());
                }
            }
        }
        bail!(
            "工具调用次数超过上限 {max_iters} (可在 config.toml 的 [llm] max_tool_iterations 调整), 本轮中止"
        )
    }

    /// 打断收尾: 给第 executed 个之后未执行的工具调用补占位结果,
    /// 追加一条打断说明, 发出 Reply 事件。消息序列保持 tool_calls <-> tool 一一对应。
    async fn abort_turn(
        &mut self,
        tx: &tokio::sync::mpsc::UnboundedSender<AgentEvent>,
        executed: usize,
    ) -> Result<()> {
        if executed > 0 {
            // 最后一条 assistant 消息带 tool_calls, 为未执行的调用补占位
            if let Some(calls) = self
                .messages
                .last()
                .and_then(|m| m.tool_calls.clone())
            {
                for call in calls.iter().skip(executed) {
                    self.messages.push(Message::tool(&call.id, "（用户已打断）".into()));
                }
            }
        }
        self.messages.push(Message::assistant("（已打断当前任务）"));
        let _ = tx.send(AgentEvent::Reply { text: "⏸ 已打断当前任务".into() });
        Ok(())
    }

    fn accumulate(&mut self, usage: &Usage) {
        self.usage.prompt_tokens += usage.prompt_tokens;
        self.usage.completion_tokens += usage.completion_tokens;
        self.usage.total_tokens += usage.total_tokens;
        self.calls += 1;
    }

    fn check_budget(&self) -> Result<()> {
        let budget = self.llm_cfg.token_budget;
        if budget > 0 && self.usage.total_tokens >= budget {
            bail!(
                "已达到 token 预算 ({budget}), 为避免超支本轮中止。可在 config.toml 调整 token_budget"
            );
        }
        Ok(())
    }

    fn trim_context(&mut self) {
        let budget_chars = (self.llm_cfg.context_length as usize).saturating_mul(3);
        let estimated = |msgs: &[Message]| -> usize {
            msgs.iter()
                .map(|m| {
                    m.content.as_deref().map(str::len).unwrap_or(0)
                        + m.tool_calls
                            .as_ref()
                            .map(|t| {
                                t.iter()
                                    .map(|c| c.function.arguments.len())
                                    .sum::<usize>()
                            })
                            .unwrap_or(0)
                        + 24
                })
                .sum()
        };
        while estimated(&self.messages) > budget_chars {
            let cut = self
                .messages
                .iter()
                .position(|m| m.role == "user")
                .filter(|&i| i > 0);
            match cut {
                Some(i) if self.messages.len() > 4 => {
                    self.messages.drain(1..=i);
                }
                _ => break,
            }
        }
    }

    pub fn cost(&self) -> f64 {
        self.usage.prompt_tokens as f64 / 1e6 * self.llm_cfg.price_input_per_m
            + self.usage.completion_tokens as f64 / 1e6 * self.llm_cfg.price_output_per_m
    }

    pub fn usage_summary(&self) -> String {
        format!(
            "调用 {} 次 | 输入 {} tok | 输出 {} tok | 合计 {} tok | 费用 ${:.4} (预算 {} tok)",
            self.calls,
            self.usage.prompt_tokens,
            self.usage.completion_tokens,
            self.usage.total_tokens,
            self.cost(),
            if self.llm_cfg.token_budget == 0 { "不限".to_string() } else { self.llm_cfg.token_budget.to_string() }
        )
    }
}

/// 轮询打断标记 (200ms), 供 tokio::select! 与 LLM 调用竞争
async fn wait_interrupt(flag: &Arc<AtomicBool>) {
    loop {
        if flag.load(Ordering::Relaxed) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

fn truncate(s: &str, max: usize) -> &str {
    if s.len() <= max {
        s
    } else {
        let mut end = max;
        while !s.is_char_boundary(end) {
            end -= 1;
        }
        &s[..end]
    }
}
