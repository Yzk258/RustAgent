use crate::prelude::*;
use std::sync::atomic::AtomicI64;

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
    /// 工具内部的阶段性进展 (如 "正在收集 mod sodium (2/8)"), 原地更新展示;
    /// current/total 存在时前端可渲染进度条
    Progress {
        text: String,
        current: Option<u64>,
        total: Option<u64>,
    },
    /// 最终回复的增量片段 (流式输出, 打字机效果)
    ReplyDelta { text: String },
    /// 最终自然语言回复 (完整文本, 紧跟在 ReplyDelta 序列之后)
    Reply { text: String },
    /// 单次 LLM API 调用返回的 token 用量 (每次 chat_stream 成功后发出, 供前端实时展示)
    LlmUsage {
        prompt_tokens: u64,
        completion_tokens: u64,
        total_tokens: u64,
    },
}

const SYSTEM_PROMPT: &str = "你是 Minecraft 模组管理助手 RustAgent。核心原则: 你负责理解与沟通, 正确性由工具保证 —— 绝不凭记忆推荐 mod, 一切 mod 数据必须来自工具返回的真实 API 数据。

工作流程:
1. 理解需求: 确认 Minecraft 版本、加载器(fabric/forge/neoforge/quilt)和游玩偏好。用户没说清楚的先问。用户用中文描述主题没关系, 搜索工具会自动转换关键词。特别注意: 若用户消息开头带 [界面预设: ...], 说明版本/加载器/候选数量已在界面选好, 视为用户确认, 直接采用, 绝不要再追问这些信息; 预设中的候选数量应作为 search_mods 的 limit 参数 (单次对话上限 20)。
2. 推荐前先调用 get_user_profile 了解用户口味, 再调用 search_mods 搜索(必须传 game_version 和 loader)。
3. 把候选 mod 以列表呈现: 名称、一句话推荐理由(结合用户口味)、下载量。先不下载, 请用户挑选, 不要替用户做决定。
4. 用户确认后调用 build_modpack 生成整合包(自动补全前置依赖并检测冲突), 报告输出路径与冲突详情。生成的 .mrpack 可拖入 PCL2 等启动器直接安装。限制说明(用户触及时主动解释): 单次对话找包/挑选上限 20 个; 单包用户所选 mod 上限 100 个(前置依赖自动补全与报错修复补入不计入) —— 为考虑轻量化, 敬请谅解, 可建议用户分多轮组包。
5. 用户表达喜欢/不喜欢时调用 record_feedback 记录; 用户想看点新的时调用 recommend_new_mods。
6. 搜索无结果时换个关键词重试, 而不是放弃。
7. 用户贴出启动器报错(如缺少某依赖、mod 不兼容)时: 从报错中提取缺失 mod 的名称, 用 search_mods 找到 slug, 调用 repair_pack 把它补进原整合包, 并告知用户重新拖入启动器安装。
8. CurseForge 独占 mod: Modrinth 搜索无结果时 search_mods 会自动尝试 CurseForge 点名查询(需服务器开启 curseforge 支持), CF 候选带 source 为 curseforge 的标记, 组包时放入 build_modpack 的 cf_mods 参数(不是 mod_slugs)。未开启时告知用户可在 config.toml 的 [curseforge] 打开。OptiFine 等不提供任何接口的 mod 只能引导用户去官网手动下载。
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
    /// 当前自动保存文件路径 (None = 本会话尚未保存过); 加载/导入历史后重置为 None
    pub session_file: Option<String>,
}

/// 组装一个全新的 Agent (CLI 与 Web UI 共用的构造入口)。
/// 新增底层客户端时在这里统一接线。interrupt 由调用方持有 (UI 换 agent 时复用同一标记)。
pub async fn new_agent(cfg: &crate::config::Config, interrupt: Arc<AtomicBool>) -> Result<Agent> {
    let llm = LlmClient::new(cfg.llm.clone())?;
    let modrinth = crate::modrinth::ModrinthClient::new()?;
    let cf = cfg
        .curseforge
        .enabled
        .then(crate::curseforge::CfClient::new);
    let registry = ToolRegistry::new(modrinth, cf, &cfg.output.download_dir, cfg.db_path());
    Ok(Agent::new(llm, registry, cfg.llm.clone(), interrupt))
}

impl Agent {
    pub fn new(
        llm: LlmClient,
        tools: ToolRegistry,
        llm_cfg: LlmConfig,
        interrupt: Arc<AtomicBool>,
    ) -> Self {
        Self {
            llm,
            tools,
            llm_cfg,
            interrupt,
            messages: vec![Message::system(SYSTEM_PROMPT)],
            usage: Usage::default(),
            calls: 0,
            session_file: None,
        }
    }

    fn interrupted(&self) -> bool {
        self.interrupt.load(Ordering::Relaxed)
    }

    /// 热更新 LLM 配置 (Web 设置窗口保存时调用): 重建客户端 + 刷新预算/上下文参数。
    /// 会话历史与用量统计保留, 下一轮对话即用新配置。
    pub fn update_llm(&mut self, cfg: LlmConfig) -> Result<()> {
        self.llm = LlmClient::new(cfg.clone())?;
        self.llm_cfg = cfg;
        Ok(())
    }

    /// 设置窗口热切换 CurseForge 支持: 会话历史保留, 下一轮对话即生效
    pub fn update_curseforge(&mut self, enabled: bool) {
        self.tools
            .set_cf(enabled.then(crate::curseforge::CfClient::new));
    }

    /// CLI 入口: 与旧版行为一致, 在终端打印工具调用与最终回复。
    /// 内部复用 run_turn_with, 通过通道接收事件再打印, 保证两端行为同步。
    pub async fn run_turn(&mut self, input: &str) -> Result<()> {
        // 创建事件通道, 并启动一个打印任务消费事件
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<AgentEvent>();
        let printer = tokio::spawn(async move {
            use crate::cli::{paint, ACCENT, DIM, GREEN, RED};
            let mut pp = crate::cli::ProgressPrinter::new();
            let mut streaming = false; // 本轮回复是否已在逐字输出
            while let Some(ev) = rx.recv().await {
                match ev {
                    AgentEvent::ToolCall { name, args } => {
                        streaming = false;
                        pp.line(&format!(
                            "  {} {}({})",
                            paint(DIM, "⚙"),
                            paint(ACCENT, &name),
                            truncate(&args, 70)
                        ));
                    }
                    AgentEvent::ToolResult { name, ok } => {
                        if ok {
                            pp.line(&format!("  {} {}", paint(GREEN, "✓"), paint(DIM, &name)));
                        } else {
                            pp.line(&format!("  {} {}", paint(RED, "✗"), name));
                        }
                    }
                    AgentEvent::Progress {
                        text,
                        current,
                        total,
                    } => {
                        pp.progress(&text, current, total);
                    }
                    AgentEvent::ReplyDelta { text } => {
                        pp.delta(&text, &mut streaming);
                    }
                    AgentEvent::Reply { text } => {
                        pp.reply_end(&text, streaming);
                        streaming = false;
                    }
                    AgentEvent::LlmUsage { .. } => { /* 用量事件: Web 侧栏用, CLI 不打印 */
                    }
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

            // LLM 调用 watchdog: 超过 3s 时周期发 "模型思考中… (Ns)" 进展事件,
            // 让 CLI/Web 在 LLM 空窗期也有实时反馈 (LLM 调用通常占一轮的大头)
            let llm_wd_tx = tx.clone();
            let llm_watchdog = tokio::spawn(async move {
                let start = std::time::Instant::now();
                loop {
                    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                    if start.elapsed().as_millis() > 3000 {
                        let _ = llm_wd_tx.send(AgentEvent::Progress {
                            text: format!("模型思考中… ({}s)", start.elapsed().as_secs()),
                            current: None,
                            total: None,
                        });
                    }
                }
            });
            // LLM 流式调用: 文本增量即时转成 ReplyDelta 事件 (打字机效果),
            // 超过 3s 无响应时 watchdog 周期发 "模型思考中… (Ns)" (工具调用分片
            // 阶段没有文本增量, 依然需要 watchdog 兜底)
            let delta_tx = tx.clone();
            let stream_result = tokio::select! {
                r = self.llm.chat_stream(
                    self.messages.clone(),
                    Some(crate::tools::ToolRegistry::defs()),
                    |d| {
                        let _ = delta_tx.send(AgentEvent::ReplyDelta { text: d.to_string() });
                    },
                ) => r,
                _ = wait_interrupt(&self.interrupt) => {
                    llm_watchdog.abort();
                    return self.abort_turn(tx, 0).await;
                }
            };
            let result = match stream_result {
                Ok(v) => v,
                Err(e) => {
                    llm_watchdog.abort();
                    return Err(e);
                }
            };
            llm_watchdog.abort();
            self.accumulate(&result.usage);
            let u = &result.usage;
            let _ = tx.send(AgentEvent::LlmUsage {
                prompt_tokens: u.prompt_tokens,
                completion_tokens: u.completion_tokens,
                total_tokens: u.total_tokens,
            });
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
                        // 长任务上下文: 工具内部上报进展 + 在循环间隙响应打断
                        // 注意: ptx 直接 move 进 ctx, 全局唯一的进展通道发送端随 ctx
                        // drop 而关闭; 若再留一个 clone, 通道永远不关, forwarder.await
                        // 会死等导致整轮对话卡死 (已踩坑)
                        let (ptx, mut prx) =
                            tokio::sync::mpsc::unbounded_channel::<crate::tools::ProgressUpdate>();
                        let last_activity = Arc::new(AtomicI64::new(now_ms()));
                        let ctx = crate::tools::TaskCtx {
                            progress: Some(ptx),
                            interrupt: Some(self.interrupt.clone()),
                        };
                        let fwd_tx = tx.clone();
                        let fwd_last = last_activity.clone();
                        let forwarder = tokio::spawn(async move {
                            while let Some(u) = prx.recv().await {
                                fwd_last.store(now_ms(), Ordering::Relaxed);
                                let _ = fwd_tx.send(AgentEvent::Progress {
                                    text: u.text,
                                    current: u.current,
                                    total: u.total,
                                });
                            }
                        });
                        // 3s 静默兜底: 工具超过 3s 未上报任何进展时周期性提示仍在执行,
                        // 保证 "超过 3 秒的任务必有实时反馈" (真实进展会刷新 last_activity)
                        let wd_tx = tx.clone();
                        let wd_last = last_activity.clone();
                        let wd_name = name.clone();
                        let watchdog = tokio::spawn(async move {
                            let start = std::time::Instant::now();
                            loop {
                                tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                                let now = now_ms();
                                if start.elapsed().as_millis() > 3000
                                    && now - wd_last.load(Ordering::Relaxed) > 3000
                                {
                                    wd_last.store(now, Ordering::Relaxed);
                                    let _ = wd_tx.send(AgentEvent::Progress {
                                        text: format!(
                                            "{wd_name} 仍在执行… ({}s)",
                                            start.elapsed().as_secs()
                                        ),
                                        current: None,
                                        total: None,
                                    });
                                }
                            }
                        });
                        let (result, ok, tool_interrupted) = match self
                            .tools
                            .execute(name, &call.function.arguments, &ctx)
                            .await
                        {
                            Ok(v) => (v.to_string(), true, false),
                            Err(e) => {
                                let s = format!("{e:#}");
                                let interrupted = s.contains(crate::tools::TOOL_INTERRUPTED);
                                (
                                    serde_json::json!({ "error": s }).to_string(),
                                    false,
                                    interrupted,
                                )
                            }
                        };
                        drop(ctx); // 关闭进展通道的唯一发送端, 等转发任务排空
                        let _ = forwarder.await;
                        watchdog.abort();
                        // 发出工具结果事件
                        let _ = tx.send(AgentEvent::ToolResult {
                            name: name.clone(),
                            ok,
                        });
                        self.messages.push(Message::tool(&call.id, result));
                        // 工具内部被打断: 不再回传 LLM 浪费 token, 直接收尾
                        if tool_interrupted {
                            return self.abort_turn(tx, i + 1).await;
                        }
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
            if let Some(calls) = self.messages.last().and_then(|m| m.tool_calls.clone()) {
                for call in calls.iter().skip(executed) {
                    self.messages
                        .push(Message::tool(&call.id, "（用户已打断）".into()));
                }
            }
        }
        self.messages.push(Message::assistant("（已打断当前任务）"));
        let _ = tx.send(AgentEvent::Reply {
            text: "⏸ 已打断当前任务".into(),
        });
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
                            .map(|t| t.iter().map(|c| c.function.arguments.len()).sum::<usize>())
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
            if self.llm_cfg.token_budget == 0 {
                "不限".to_string()
            } else {
                self.llm_cfg.token_budget.to_string()
            }
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

/// 当前毫秒时间戳 (watchdog 静默检测用)
fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
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
