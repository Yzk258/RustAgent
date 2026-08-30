use anyhow::{bail, Result};

use crate::config::LlmConfig;
use crate::llm::{LlmClient, Message, Usage};
use crate::tools::ToolRegistry;

const MAX_TOOL_ITERATIONS: usize = 8;

const SYSTEM_PROMPT: &str = "你是 Minecraft 模组管理助手 RustAgent。核心原则: 你负责理解与沟通, 正确性由工具保证 —— 绝不凭记忆推荐 mod, 一切 mod 数据必须来自工具返回的真实 API 数据。

工作流程:
1. 理解需求: 确认 Minecraft 版本、加载器(fabric/forge/neoforge)和游玩偏好。用户没说清楚的先问。用户用中文描述主题没关系, 搜索工具会自动转换关键词。
2. 推荐前先调用 get_user_profile 了解用户口味, 再调用 search_mods 搜索(必须传 game_version 和 loader)。
3. 把候选 mod 以列表呈现: 名称、一句话推荐理由(结合用户口味)、下载量。先不下载, 请用户挑选, 不要替用户做决定。
4. 用户确认后调用 build_modpack 生成整合包(自动补全前置依赖并检测冲突), 报告输出路径与冲突详情。生成的 .mrpack 可拖入 PCL2 等启动器直接安装。
5. 用户表达喜欢/不喜欢时调用 record_feedback 记录; 用户想看点新的时调用 recommend_new_mods。
6. 搜索无结果时换个关键词重试, 而不是放弃。
7. 用户贴出启动器报错(如缺少某依赖、mod 不兼容)时: 从报错中提取缺失 mod 的名称, 用 search_mods 找到 slug, 调用 repair_pack 把它补进原整合包, 并告知用户重新拖入启动器安装。
始终用中文回复。同一轮内工具调用失败要向用户说明原因并给出替代方案。";

pub struct Agent {
    llm: LlmClient,
    tools: ToolRegistry,
    llm_cfg: LlmConfig,
    pub messages: Vec<Message>,
    pub usage: Usage,
    pub calls: u64,
}

impl Agent {
    pub fn new(llm: LlmClient, tools: ToolRegistry, llm_cfg: LlmConfig) -> Self {
        Self {
            llm,
            tools,
            llm_cfg,
            messages: vec![Message::system(SYSTEM_PROMPT)],
            usage: Usage::default(),
            calls: 0,
        }
    }

    pub async fn run_turn(&mut self, input: &str) -> Result<()> {
        self.messages.push(Message::user(input));

        for _ in 0..MAX_TOOL_ITERATIONS {
            self.check_budget()?;
            self.trim_context();

            let result = self
                .llm
                .chat(self.messages.clone(), Some(crate::tools::ToolRegistry::defs()))
                .await?;
            self.accumulate(&result.usage);
            let msg = result.message;

            match msg.tool_calls.clone() {
                Some(calls) if !calls.is_empty() => {
                    self.messages.push(msg);
                    for call in calls {
                        let name = &call.function.name;
                        println!("  → 工具 {name}({}...)", truncate(&call.function.arguments, 80));
                        let result = match self.tools.execute(name, &call.function.arguments).await {
                            Ok(v) => v.to_string(),
                            Err(e) => format!("{{ \"error\": \"{}\" }}", format!("{e:#}").replace('"', "'")),
                        };
                        self.messages.push(Message::tool(&call.id, result));
                    }
                }
                _ => {
                    let text = msg.content.clone().unwrap_or_default();
                    self.messages.push(msg);
                    println!("\n{text}\n");
                    return Ok(());
                }
            }
        }
        bail!("工具调用次数超过上限 {MAX_TOOL_ITERATIONS}, 本轮中止")
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
