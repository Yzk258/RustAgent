//! 原生桌面 UI (egui + eframe)。`cargo run -- desktop` 启动。
//!
//! 与 Web UI (`src/ui/static/*`) 保持同一套**格式**, 只是渲染层从 HTML/CSS 换成 egui:
//!   顶栏   品牌/版本 · 当前模型 · 连接状态 · 设置
//!   侧栏   会话 / 统计 / 已生成整合包 / 可用工具 四张卡片 (与 index.html 的 .card 一一对应)
//!   对话区 头像+气泡 (用户/助手)、`⚙` 工具行、`⏳` 进度行、`📦` mod 卡片
//!   底栏   预设工具栏 (版本/加载器/数量) + 输入栏 (发送/打断)
//! 数据来源与 Web 版一致: 直接消费 `AgentEvent`, 不启 HTTP 服务、不开浏览器。
//! 配色取自 style.css 的 `:root` 深色主题, 见下方 `skin` 模块。

use crate::agent::{new_agent, AgentEvent};
use crate::config::{Config, LlmConfig};
use crate::prelude::*;
use eframe::egui;
use std::path::Path;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

// ---------------------------------------------------------------------------
// 配色: 与 style.css 的 :root 深色主题逐项对应, 改配色只改这里
// ---------------------------------------------------------------------------
mod skin {
    use eframe::egui::Color32;
    pub const PANEL: Color32 = Color32::from_rgb(21, 24, 32); // --bg-panel
    pub const CARD: Color32 = Color32::from_rgb(22, 26, 34); // --bg-card
    pub const RAISE: Color32 = Color32::from_rgb(27, 32, 43); // --bg-raise
    pub const BUBBLE: Color32 = Color32::from_rgb(29, 35, 48); // --bg-bubble
    pub const BORDER: Color32 = Color32::from_rgb(38, 45, 58); // --border
    pub const TEXT: Color32 = Color32::from_rgb(233, 236, 242); // --text
    pub const DIM: Color32 = Color32::from_rgb(138, 147, 166); // --text-dim
    pub const ACCENT: Color32 = Color32::from_rgb(109, 141, 255); // --accent
    pub const ACCENT_SOFT: Color32 = Color32::from_rgb(34, 42, 62); // --accent-soft (半透明压在深底上的近似色)
    pub const GREEN: Color32 = Color32::from_rgb(52, 211, 153); // --green
    pub const RED: Color32 = Color32::from_rgb(248, 113, 113); // --red
    pub const YELLOW: Color32 = Color32::from_rgb(251, 191, 36); // --yellow
}

/// 中文字体: egui 自带字体不含 CJK, 不装就全是"豆腐块"方框。
/// 依次尝试系统里的常见中文字体, 第一个能读到的就用; 全失败也只是字丑, 不影响运行。
/// 返回实际采用的字体路径 (便于自检/测试确认真的装上了)。
fn install_fonts(ctx: &egui::Context) -> Option<String> {
    const CANDIDATES: [&str; 6] = [
        r"C:\Windows\Fonts\msyh.ttc",   // 微软雅黑 (Win8+ 默认 UI 字体)
        r"C:\Windows\Fonts\msyh.ttf",
        r"C:\Windows\Fonts\simhei.ttf", // 黑体
        r"C:\Windows\Fonts\Deng.ttf",   // 等线
        r"C:\Windows\Fonts\simsun.ttc", // 宋体
        "/System/Library/Fonts/PingFang.ttc",
    ];
    let (path, bytes) = CANDIDATES.iter().find_map(|p| std::fs::read(p).ok().map(|b| (*p, b)))?;
    let mut fonts = egui::FontDefinitions::default();
    fonts
        .font_data
        .insert("cjk".to_owned(), egui::FontData::from_owned(bytes));
    for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
        if let Some(list) = fonts.families.get_mut(&family) {
            list.insert(0, "cjk".to_owned()); // 放最前: 中文优先命中, 拉丁仍可用原字体兜底
        }
    }
    ctx.set_fonts(fonts);
    Some(path.to_string())
}

/// 整体视觉: 深色玻璃面板 + 14/9 圆角 + 蓝紫强调, 对应 style.css 的变量体系
fn install_skin(ctx: &egui::Context) {
    install_fonts(ctx);
    let mut v = egui::Visuals::dark();
    v.panel_fill = skin::PANEL;
    v.window_fill = skin::CARD;
    v.extreme_bg_color = skin::RAISE;
    v.faint_bg_color = skin::RAISE;
    v.override_text_color = Some(skin::TEXT);
    v.hyperlink_color = skin::ACCENT;
    v.window_stroke = egui::Stroke::new(1.0_f32, skin::BORDER);
    v.selection.bg_fill = skin::ACCENT_SOFT;
    v.selection.stroke = egui::Stroke::new(1.0_f32, skin::ACCENT);
    v.widgets.noninteractive.bg_fill = skin::CARD;
    v.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, skin::BORDER);
    v.widgets.inactive.bg_fill = skin::RAISE;
    v.widgets.inactive.weak_bg_fill = skin::RAISE;
    v.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, skin::BORDER);
    v.widgets.hovered.bg_fill = skin::ACCENT_SOFT;
    v.widgets.hovered.weak_bg_fill = skin::ACCENT_SOFT;
    v.widgets.active.bg_fill = skin::ACCENT_SOFT;
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.rounding = egui::Rounding::same(9.0); // --radius-sm
    }
    v.window_rounding = egui::Rounding::same(14.0); // --radius
    ctx.set_visuals(v);
}

// ---------------------------------------------------------------------------
// 对话区消息模型: 与 app.js 的消息种类一一对应
// ---------------------------------------------------------------------------

/// 工具行状态: 调用中转圈 → 完成/失败染色
#[derive(Clone, Copy, PartialEq)]
enum ToolState {
    Pending,
    Ok,
    Fail,
}

/// mod 卡片 (来自 search_mods / build_modpack 的工具返回数据, 见 app.js 的 extractMods)
#[derive(Clone)]
struct ModCard {
    title: String,
    slug: String,
    description: String,
    downloads: u64,
    categories: Vec<String>,
    url: String,
    gallery: usize,
    taste: Option<f64>,
}

enum Msg {
    /// 用户消息 (气泡靠右, 头像"我")
    User(String),
    /// 助手回复 (气泡靠左, 头像"⛏"; ReplyDelta 流式追加)
    Assistant(String),
    /// `⚙ 调用工具 x …` → 完成/失败
    Tool { name: String, state: ToolState },
    /// `⏳ ▰▰▱▱ 3/8 正在…`: 同一工具调用内原地更新
    Progress { text: String, current: Option<u64>, total: Option<u64> },
    /// mod 卡片网格
    Cards(Vec<ModCard>),
    /// 系统提示 (出错/打断/欢迎)
    Notice(String),
}

/// 对话区状态机: 把 `AgentEvent` 序列按 Web 版的规则拼成消息列表。
/// 单独抽一层的原因: 这里的顺序规则 (工具行原地更新、进度行用完即撤、
/// 工具调用后另起气泡) 不需要窗口就能单元测试, 见文件末尾的 tests。
#[derive(Default)]
struct Chat {
    messages: Vec<Msg>,
    /// 当前流式回复所在的消息下标 (工具调用后置 None, 让后续文本另起一个气泡)
    cur_reply: Option<usize>,
    /// 当前进度行的下标 (工具结果到达时移除, 与 app.js 的 clearProgress 一致)
    cur_progress: Option<usize>,
    /// 推理型模型的思考片段 (只用于状态栏活性提示, 不进对话)
    thinking_tail: String,
    busy: bool,
    /// 本轮已收 LlmUsage 增量 (快照到达后清零), 让统计卡片在本轮内也能动
    token_delta: u64,
    /// 本轮已结束: 侧栏需要重扫 (整合包列表/口味库)
    dirty: bool,
}

impl Chat {
    fn welcome() -> Self {
        Self {
            messages: vec![Msg::Notice(
                "欢迎使用 RustAgent。\n告诉我你的 Minecraft 版本、加载器和想要的 mod，我会帮你搜索并生成整合包。".into(),
            )],
            ..Default::default()
        }
    }

    fn notice(&mut self, text: impl Into<String>) {
        self.messages.push(Msg::Notice(text.into()));
    }

    fn clear(&mut self) {
        self.messages.clear();
        self.cur_reply = None;
        self.cur_progress = None;
        self.thinking_tail.clear();
    }

    fn take_dirty(&mut self) -> bool {
        std::mem::take(&mut self.dirty)
    }

    /// 状态栏文案: 由对话状态推导 (与 Web 版的状态标签/思考行同义)
    fn status(&self) -> String {
        if !self.busy {
            return "就绪".into();
        }
        if self.thinking_tail.is_empty() {
            "正在处理…".into()
        } else {
            format!("思考中… {}", self.thinking_tail.replace('\n', " "))
        }
    }

    /// 工具结果到达后进度行就过期了 (与 app.js 的 clearProgress 一致)
    fn drop_progress(&mut self) {
        if let Some(i) = self.cur_progress.take() {
            if i + 1 == self.messages.len() {
                self.messages.remove(i);
            }
        }
    }

    /// 消费一个 Agent 事件, 规则与 app.js 的 handleEvent 一一对应
    fn apply(&mut self, ev: AgentEvent) {
        match ev {
            AgentEvent::ToolCall { name, .. } => {
                // 工具调用后的新文本要另起气泡, 否则回复会续写到工具行上面去
                self.cur_reply = None;
                self.busy = true;
                self.messages.push(Msg::Tool { name, state: ToolState::Pending });
            }
            AgentEvent::ToolResult { name, ok, result } => {
                for m in self.messages.iter_mut().rev() {
                    if let Msg::Tool { name: n, state } = m {
                        if *n == name && *state == ToolState::Pending {
                            *state = if ok { ToolState::Ok } else { ToolState::Fail };
                            break;
                        }
                    }
                }
                self.drop_progress();
                // 工具返回数据含 mod 列表时插入结构化卡片 (与 Web 版 renderModCards 同位置)
                let cards = extract_mods(result.as_ref());
                if !cards.is_empty() {
                    self.messages.push(Msg::Cards(cards));
                }
            }
            AgentEvent::Progress { text, current, total } => {
                match self.cur_progress {
                    // 同一次工具调用内的多条 progress 原地更新同一行
                    Some(i) if i + 1 == self.messages.len() => {
                        self.messages[i] = Msg::Progress { text, current, total };
                    }
                    _ => {
                        self.messages.push(Msg::Progress { text, current, total });
                        self.cur_progress = Some(self.messages.len() - 1);
                    }
                }
                self.busy = true;
            }
            AgentEvent::ReplyDelta { text } => {
                self.drop_progress();
                match self.cur_reply {
                    Some(i) => {
                        if let Some(Msg::Assistant(t)) = self.messages.get_mut(i) {
                            t.push_str(&text);
                        }
                    }
                    None => {
                        self.messages.push(Msg::Assistant(text));
                        self.cur_reply = Some(self.messages.len() - 1);
                    }
                }
                self.busy = true;
            }
            AgentEvent::ReasoningDelta { text } => {
                // 只留尾部一小段做活性提示, 避免越攒越长
                self.thinking_tail.push_str(&text);
                let tail: String = self.thinking_tail.chars().rev().take(120).collect();
                self.thinking_tail = tail.chars().rev().collect();
            }
            AgentEvent::Reply { text } => {
                self.drop_progress();
                match self.cur_reply {
                    Some(i) => {
                        if let Some(Msg::Assistant(t)) = self.messages.get_mut(i) {
                            *t = text; // 完整文本是权威版本, 直接覆盖流式累积结果
                        }
                    }
                    None => self.messages.push(Msg::Assistant(text)),
                }
                self.cur_reply = None;
                self.thinking_tail.clear();
                self.busy = false;
                self.dirty = true;
            }
            AgentEvent::LlmUsage { total_tokens, .. } => {
                self.token_delta = self.token_delta.saturating_add(total_tokens);
            }
        }
    }
}

/// 下载量格式化: 12.3M / 5.6K (与 app.js 的 fmtDownloads 一致)
fn fmt_downloads(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}

/// `▰▱` 20 格进度条 (与 app.js 的 progressPrefix 一致)
fn progress_prefix(current: Option<u64>, total: Option<u64>) -> String {
    let Some(total) = total.filter(|t| *t > 0) else {
        return String::new();
    };
    const W: usize = 20;
    let filled = ((current.unwrap_or(0).min(total)) as f64 / total as f64 * W as f64).round() as usize;
    format!(
        "{}{} {}/{} ",
        "▰".repeat(filled),
        "▱".repeat(W - filled),
        current.unwrap_or(0),
        total
    )
}

/// 从工具返回的 JSON 里抽 mod 列表。约定与前端 extractMods 相同:
/// - search_mods:  `{ mods: [{slug,title,description,downloads,categories,icon_url,url,gallery,taste_score}] }`
/// - build_modpack: `{ mods: [{slug,version,filename,size_mb}] }` (没有标题/图标, 用 slug + 文件名兜底)
fn extract_mods(result: Option<&serde_json::Value>) -> Vec<ModCard> {
    let Some(mods) = result.and_then(|r| r.get("mods")).and_then(|m| m.as_array()) else {
        return Vec::new();
    };
    mods.iter()
        .filter_map(|m| {
            let slug = m.get("slug").and_then(|v| v.as_str()).unwrap_or("").to_string();
            // 组包结果的条目只有 filename/size_mb, 标题缺失时用 slug 顶替
            let title = m
                .get("title")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| slug.clone());
            if title.is_empty() {
                return None;
            }
            let url = m
                .get("url")
                .and_then(|v| v.as_str())
                .map(str::to_string)
                .unwrap_or_else(|| format!("https://modrinth.com/mod/{slug}"));
            Some(ModCard {
                title,
                slug,
                description: m.get("description").and_then(|v| v.as_str()).unwrap_or("").to_string(),
                downloads: m.get("downloads").and_then(|v| v.as_u64()).unwrap_or(0),
                categories: m
                    .get("categories")
                    .and_then(|v| v.as_array())
                    .map(|a| a.iter().filter_map(|c| c.as_str().map(str::to_string)).collect())
                    .unwrap_or_default(),
                url,
                gallery: m.get("gallery").and_then(|v| v.as_array()).map(|g| g.len()).unwrap_or(0),
                taste: m.get("taste_score").and_then(|v| v.as_f64()),
            })
        })
        .collect()
}

// ---------------------------------------------------------------------------
// 侧栏数据 (与 /api/packs、/api/profile、/api/tools 同源, 这里直接在进程内取)
// ---------------------------------------------------------------------------

struct PackEntry {
    name: String,
    size_kb: u64,
    modified: String,
}

/// 扫描输出目录下的 .mrpack (对应 api::packs)
fn scan_packs(dir: &Path) -> Vec<PackEntry> {
    let mut out = Vec::new();
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|x| x == "mrpack") {
                let meta = entry.metadata().ok();
                out.push(PackEntry {
                    name: path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
                    size_kb: meta.as_ref().map(|m| m.len() / 1024).unwrap_or(0),
                    modified: meta
                        .as_ref()
                        .and_then(|m| m.modified().ok())
                        .map(|t| chrono::DateTime::<chrono::Local>::from(t).format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_default(),
                });
            }
        }
    }
    out.sort_by(|a, b| b.modified.cmp(&a.modified));
    out
}

/// 用户口味库摘要 (对应 /api/profile): 反馈统计 + 标签权重
fn load_taste(db_path: &str) -> (String, Vec<(String, f64)>) {
    let db = crate::storage::database::UserDatabase::load(db_path);
    let mut weights: Vec<(String, f64)> = db.tag_weights().into_iter().collect();
    weights.sort_by(|a, b| b.1.abs().partial_cmp(&a.1.abs()).unwrap_or(std::cmp::Ordering::Equal));
    weights.truncate(12);
    (db.summary(), weights)
}

/// worker 线程每轮结束后回传给 UI 的用量快照 (对应 /api/info)
#[derive(Default, Clone)]
struct Snapshot {
    calls: u64,
    total_tokens: u64,
    cost: f64,
    usage_summary: String,
    session_file: Option<String>,
}

// ---------------------------------------------------------------------------
// 线程通信: UI 发命令, worker 发事件
// ---------------------------------------------------------------------------

enum Command {
    Chat(String),
    NewSession,
    UpdateLlm(Box<LlmConfig>),
    UpdateCurseforge(bool),
}

enum UiEvent {
    Agent(AgentEvent),
    Snapshot(Snapshot),
    /// worker 侧的一行提示 (初始化失败/新会话就绪等)
    Notice(String),
}

/// 起 worker 线程: 独占 Agent (与 Web 版一样"同一时刻只有一件事在做"),
/// 事件转发给 UI; UI 线程只读事件、发命令。
pub fn run(cfg: Config) -> Result<()> {
    let download_dir = std::path::absolute(&cfg.output.download_dir)
        .unwrap_or_else(|_| std::path::PathBuf::from(&cfg.output.download_dir))
        .display()
        .to_string();
    let db_path = cfg.db_path();
    let config_path = "config.toml".to_string();
    let model = cfg.llm.model.clone();
    let settings = SettingsForm::from(&cfg.llm, cfg.curseforge.enabled);
    // 设置弹窗的"兜底值"要来自当前生效配置, 而不是表单自身 —— 否则把某格清空会静默变成默认值
    let live_llm = cfg.llm.clone();

    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
    let (event_tx, event_rx) = mpsc::channel::<UiEvent>();
    let interrupt = Arc::new(AtomicBool::new(false));
    let worker_interrupt = interrupt.clone();
    thread::spawn(move || worker(cfg, cmd_rx, event_tx, worker_interrupt));

    eframe::run_native(
        "RustAgent · MC 模组管理助手",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1180.0, 800.0])
                .with_min_inner_size([880.0, 600.0])
                .with_title("RustAgent · MC 模组管理助手"),
            ..Default::default()
        },
        Box::new(move |cc| {
            install_skin(&cc.egui_ctx);
            Ok(Box::new(DesktopApp::new(
                cmd_tx,
                event_rx,
                interrupt,
                download_dir,
                db_path,
                config_path,
                model,
                settings,
                live_llm,
            )))
        }),
    )
    .map_err(|e| anyhow!("桌面窗口启动失败: {e}"))
}

/// worker: 拥有 Agent, 顺序处理命令, 把事件与用量快照转发给 UI
fn worker(mut cfg: Config, cmd_rx: Receiver<Command>, tx: Sender<UiEvent>, interrupt: Arc<AtomicBool>) {
    let runtime = match tokio::runtime::Runtime::new() {
        Ok(rt) => rt,
        Err(e) => {
            let _ = tx.send(UiEvent::Notice(format!("启动失败: {e:#}")));
            return;
        }
    };
    runtime.block_on(async move {
        let mut agent = match new_agent(&cfg, interrupt.clone()).await {
            Ok(a) => a,
            Err(e) => {
                let _ = tx.send(UiEvent::Notice(format!("初始化 Agent 失败: {e:#}")));
                return;
            }
        };
        let _ = tx.send(UiEvent::Snapshot(snapshot(&agent)));
        while let Ok(cmd) = cmd_rx.recv() {
            match cmd {
                Command::Chat(input) => {
                    let (ev_tx, mut ev_rx) = tokio::sync::mpsc::unbounded_channel();
                    let forward = tx.clone();
                    let forwarder = tokio::spawn(async move {
                        while let Some(ev) = ev_rx.recv().await {
                            if forward.send(UiEvent::Agent(ev)).is_err() {
                                break; // 窗口已关
                            }
                        }
                    });
                    if let Err(e) = agent.run_turn_with(&input, &ev_tx).await {
                        let _ = tx.send(UiEvent::Notice(format!("执行失败: {e:#}")));
                    }
                    drop(ev_tx);
                    let _ = forwarder.await;
                    let _ = tx.send(UiEvent::Snapshot(snapshot(&agent)));
                }
                Command::NewSession => {
                    interrupt.store(true, Ordering::Relaxed);
                    match new_agent(&cfg, interrupt.clone()).await {
                        Ok(a) => {
                            agent = a;
                            let _ = tx.send(UiEvent::Notice("已开启新会话".into()));
                            let _ = tx.send(UiEvent::Snapshot(snapshot(&agent)));
                        }
                        Err(e) => {
                            let _ = tx.send(UiEvent::Notice(format!("新会话失败: {e:#}")));
                        }
                    }
                }
                Command::UpdateLlm(llm) => {
                    cfg.llm = (*llm).clone();
                    match agent.update_llm((*llm).clone()) {
                        Ok(()) => {
                            let _ = tx.send(UiEvent::Notice(format!("设置已生效, 当前模型: {}", llm.model)));
                            let _ = tx.send(UiEvent::Snapshot(snapshot(&agent)));
                        }
                        Err(e) => {
                            let _ = tx.send(UiEvent::Notice(format!("设置未生效: {e:#}")));
                        }
                    }
                }
                Command::UpdateCurseforge(enabled) => {
                    cfg.curseforge.enabled = enabled;
                    agent.update_curseforge(enabled);
                    let _ = tx.send(UiEvent::Notice(format!(
                        "CurseForge 已{}",
                        if enabled { "开启" } else { "关闭" }
                    )));
                }
            }
        }
    });
}

fn snapshot(agent: &crate::agent::Agent) -> Snapshot {
    Snapshot {
        calls: agent.calls,
        total_tokens: agent.usage.total_tokens,
        cost: agent.cost(),
        usage_summary: agent.usage_summary(),
        session_file: agent.session_file.clone(),
    }
}

// ---------------------------------------------------------------------------
// 设置表单 (对应 Web 版的设置弹窗, 字段与 config.toml 的 [llm] 段一致)
// ---------------------------------------------------------------------------

struct SettingsForm {
    base_url: String,
    api_key: String,
    model: String,
    context_length: String,
    price_input: String,
    price_output: String,
    token_budget: String,
    max_tools: String,
    thinking: String,
    curseforge: bool,
}

impl SettingsForm {
    fn from(llm: &LlmConfig, curseforge: bool) -> Self {
        Self {
            base_url: llm.base_url.clone(),
            api_key: llm.api_key.clone(),
            model: llm.model.clone(),
            context_length: llm.context_length.to_string(),
            price_input: llm.price_input_per_m.to_string(),
            price_output: llm.price_output_per_m.to_string(),
            token_budget: llm.token_budget.to_string(),
            max_tools: llm.max_tool_iterations.to_string(),
            thinking: llm.thinking.clone().unwrap_or_default(),
            curseforge,
        }
    }

    /// 解析回 LlmConfig: 解析不了/留空的字段沿用旧值, 不让手滑清空把配置写坏
    fn to_llm(&self, old: &LlmConfig) -> LlmConfig {
        let num = |s: &str, fallback: u64| s.trim().parse::<u64>().unwrap_or(fallback);
        let price = |s: &str, fallback: f64| s.trim().parse::<f64>().unwrap_or(fallback);
        LlmConfig {
            base_url: self.base_url.trim().to_string(),
            api_key: self.api_key.trim().to_string(),
            model: self.model.trim().to_string(),
            context_length: num(&self.context_length, old.context_length),
            price_input_per_m: price(&self.price_input, old.price_input_per_m),
            price_output_per_m: price(&self.price_output, old.price_output_per_m),
            token_budget: num(&self.token_budget, old.token_budget),
            max_tool_iterations: num(&self.max_tools, old.max_tool_iterations as u64) as u32,
            thinking: match self.thinking.trim() {
                "" => None,
                s => Some(s.to_string()),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// 主界面
// ---------------------------------------------------------------------------

struct DesktopApp {
    commands: Sender<Command>,
    events: Receiver<UiEvent>,
    interrupt: Arc<AtomicBool>,

    // 对话区 (消息组装在 Chat 里, 这里只负责画)
    chat: Chat,

    // 输入区
    input: String,
    preset_version: String,
    preset_loader: String,
    preset_limit: u32,

    // 侧栏
    download_dir: String,
    db_path: String,
    config_path: String,
    model: String,
    packs: Vec<PackEntry>,
    taste_summary: String,
    tag_weights: Vec<(String, f64)>,
    tools: Vec<(String, String)>,
    snapshot: Snapshot,

    // 设置
    settings_open: bool,
    settings: SettingsForm,
    settings_msg: String,
    /// 当前生效的 LLM 配置: 表单里留空/填错时用它兜底
    live_llm: LlmConfig,
}

impl DesktopApp {
    #[allow(clippy::too_many_arguments)]
    fn new(
        commands: Sender<Command>,
        events: Receiver<UiEvent>,
        interrupt: Arc<AtomicBool>,
        download_dir: String,
        db_path: String,
        config_path: String,
        model: String,
        settings: SettingsForm,
        live_llm: LlmConfig,
    ) -> Self {
        let (taste_summary, tag_weights) = load_taste(&db_path);
        let tools = crate::tools::ToolRegistry::defs()
            .iter()
            .map(|d| (d.function.name.clone(), d.function.description.clone()))
            .collect();
        let packs = scan_packs(Path::new(&download_dir));
        Self {
            commands,
            events,
            interrupt,
            chat: Chat::welcome(),
            input: String::new(),
            preset_version: String::new(),
            preset_loader: "auto".into(),
            preset_limit: 0,
            download_dir,
            db_path,
            config_path,
            model,
            packs,
            taste_summary,
            tag_weights,
            tools,
            snapshot: Snapshot::default(),
            settings_open: false,
            settings,
            settings_msg: String::new(),
            live_llm,
        }
    }

    /// 发消息: 把预设工具栏拼成前缀一起发出去 (与 Web 版 preset_prefix 同一实现)
    fn submit(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() || self.chat.busy {
            return;
        }
        let gv = (!self.preset_version.trim().is_empty()).then(|| self.preset_version.trim().to_string());
        let loader = (self.preset_loader != "auto").then(|| self.preset_loader.clone());
        let limit = (self.preset_limit > 0).then_some(self.preset_limit);
        let payload = match crate::pipeline::preset_prefix(gv.as_deref(), loader.as_deref(), limit) {
            Some(prefix) => format!("{prefix}{text}"),
            None => text.clone(),
        };

        self.chat.messages.push(Msg::User(text));
        self.chat.cur_reply = None;
        self.chat.drop_progress();
        self.chat.busy = true;
        self.input.clear();
        let _ = self.commands.send(Command::Chat(payload));
    }

    /// 消费 worker 事件: 消息组装交给 Chat, 快照与侧栏在这里更新
    fn drain_events(&mut self) {
        while let Ok(ev) = self.events.try_recv() {
            match ev {
                UiEvent::Notice(text) => {
                    self.chat.notice(text);
                    self.refresh_sidebar();
                }
                UiEvent::Snapshot(s) => {
                    self.snapshot = s;
                    self.chat.token_delta = 0;
                    self.refresh_sidebar();
                }
                UiEvent::Agent(ev) => self.chat.apply(ev),
            }
        }
        if self.chat.take_dirty() {
            self.refresh_sidebar();
        }
    }

    fn refresh_sidebar(&mut self) {
        self.packs = scan_packs(Path::new(&self.download_dir));
        let (summary, weights) = load_taste(&self.db_path);
        self.taste_summary = summary;
        self.tag_weights = weights;
    }

    // ---------------- 顶栏: 品牌 / 模型 / 状态 / 设置 ----------------
    fn ui_topbar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("topbar")
            .exact_height(46.0)
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(egui::RichText::new("⛏ RustAgent · MC 模组管理助手").size(15.0).strong());
                    ui.label(egui::RichText::new(format!("v{}", env!("CARGO_PKG_VERSION"))).size(11.0).color(skin::DIM));
                    ui.add_space(10.0);
                    ui.label(egui::RichText::new(&self.model).size(12.0).color(skin::DIM));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("⚙ 设置").clicked() {
                            self.settings_open = !self.settings_open;
                            self.settings_msg.clear();
                        }
                        let (dot, color) = if self.chat.busy {
                            ("●", skin::YELLOW)
                        } else {
                            ("●", skin::GREEN)
                        };
                        let status: String = self.chat.status().chars().take(60).collect();
                        ui.label(egui::RichText::new(status).size(12.0));
                        ui.label(egui::RichText::new(dot).color(color));
                    });
                });
            });
    }

    // ---------------- 侧栏: 会话 / 统计 / 已生成整合包 / 可用工具 ----------------
    fn ui_sidebar(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("sidebar")
            .exact_width(292.0)
            .resizable(false)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false; 2]).show(ui, |ui| {
                    ui.add_space(4.0);

                    // 会话
                    card(ui, "会话", |ui| {
                        if ui.button("新会话").clicked() {
                            let _ = self.commands.send(Command::NewSession);
                        }
                        let file = self
                            .snapshot
                            .session_file
                            .clone()
                            .unwrap_or_else(|| "尚无对话".into());
                        dim(ui, &format!("自动保存: {file}"));
                    });

                    // 统计
                    card(ui, "统计", |ui| {
                        let summary = if self.snapshot.usage_summary.is_empty() {
                            "尚无调用".to_string()
                        } else {
                            self.snapshot.usage_summary.clone()
                        };
                        ui.label(egui::RichText::new(summary).size(12.0));
                        dim(
                            ui,
                            &format!(
                                "调用 {} 次 · 合计 {} tok · 费用 ${:.4}",
                                self.snapshot.calls,
                                self.snapshot.total_tokens + self.chat.token_delta,
                                self.snapshot.cost
                            ),
                        );
                        ui.add_space(2.0);
                        dim(ui, &self.taste_summary);
                        if !self.tag_weights.is_empty() {
                            ui.horizontal_wrapped(|ui| {
                                for (tag, w) in self.tag_weights.iter().take(12) {
                                    let text = format!("{tag} {w:+.1}");
                                    let color = if *w >= 0.0 { skin::GREEN } else { skin::RED };
                                    ui.label(egui::RichText::new(text).size(11.0).color(color));
                                }
                            });
                        }
                    });

                    // 已生成整合包
                    let mut open_dir = false;
                    let mut copy_path = false;
                    card(ui, "已生成整合包", |ui| {
                        ui.horizontal(|ui| {
                            if ui.button("打开目录").clicked() {
                                open_dir = true;
                            }
                            if ui.button("复制路径").clicked() {
                                copy_path = true;
                            }
                        });
                        dim(ui, &self.download_dir);
                        if self.packs.is_empty() {
                            dim(ui, "还没有生成过整合包");
                        }
                        for p in &self.packs {
                            ui.horizontal_wrapped(|ui| {
                                ui.label(egui::RichText::new(&p.name).size(12.0).family(egui::FontFamily::Monospace));
                                dim(ui, &format!("{} KB · {}", p.size_kb, p.modified));
                            });
                        }
                    });
                    if copy_path {
                        ui.output_mut(|o| o.copied_text = self.download_dir.clone());
                    }
                    if open_dir {
                        // 与 Web 版同一限制: 受限令牌(沙箱)/非交互会话下系统不允许弹资源管理器,
                        // 那时打开会失败 —— 所以界面同时提供"复制路径"兜底
                        #[cfg(windows)]
                        {
                            let _ = std::process::Command::new("explorer.exe").arg(&self.download_dir).spawn();
                        }
                        #[cfg(not(windows))]
                        {
                            let _ = std::process::Command::new("xdg-open").arg(&self.download_dir).spawn();
                        }
                    }

                    // 可用工具
                    card(ui, "可用工具", |ui| {
                        for (name, desc) in &self.tools {
                            ui.label(egui::RichText::new(name).size(12.0).color(skin::ACCENT));
                            let short: String = desc.chars().take(48).collect();
                            dim(ui, &short);
                        }
                    });
                    ui.add_space(6.0);
                });
            });
    }

    // ---------------- 对话区 ----------------
    fn ui_chat(&mut self, ctx: &egui::Context) {
        let msgs = &self.chat.messages;
        egui::CentralPanel::default().show(ctx, |ui| {
            egui::ScrollArea::vertical()
                .stick_to_bottom(true)
                .auto_shrink([false; 2])
                .show(ui, |ui| {
                    ui.add_space(8.0);
                    draw_messages(ui, msgs);
                    ui.add_space(8.0);
                });
        });
    }

    // ---------------- 底栏: 预设工具栏 + 输入栏 ----------------
    fn ui_input(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("input")
            .show(ctx, |ui| {
                ui.add_space(6.0);
                // 预设工具栏 (对应 index.html 的 #presetbar): 版本 / 加载器 / 数量
                ui.horizontal(|ui| {
                    ui.label(egui::RichText::new("预设").size(12.0).color(skin::DIM));
                    ui.add(
                        egui::TextEdit::singleline(&mut self.preset_version)
                            .desired_width(84.0)
                            .hint_text("1.21.1"),
                    );
                    egui::ComboBox::from_id_salt("preset-loader")
                        .selected_text(match self.preset_loader.as_str() {
                            "auto" => "加载器: 自动",
                            "fabric" => "加载器: Fabric",
                            "neoforge" => "加载器: NeoForge",
                            "forge" => "加载器: Forge",
                            "quilt" => "加载器: Quilt",
                            _ => "加载器: 原版",
                        })
                        .show_ui(ui, |ui| {
                            for (v, label) in [
                                ("auto", "加载器: 自动"),
                                ("fabric", "加载器: Fabric"),
                                ("neoforge", "加载器: NeoForge"),
                                ("forge", "加载器: Forge"),
                                ("quilt", "加载器: Quilt"),
                                ("vanilla", "加载器: 原版"),
                            ] {
                                ui.selectable_value(&mut self.preset_loader, v.to_string(), label);
                            }
                        });
                    egui::ComboBox::from_id_salt("preset-limit")
                        .selected_text(if self.preset_limit == 0 {
                            "数量: 自动".to_string()
                        } else {
                            format!("数量: {}", self.preset_limit)
                        })
                        .show_ui(ui, |ui| {
                            for n in [0u32, 4, 6, 8, 12, 16] {
                                let label = if n == 0 { "数量: 自动".to_string() } else { format!("数量: {n}") };
                                ui.selectable_value(&mut self.preset_limit, n, label);
                            }
                        });
                    // 让用户看到前缀到底会拼成什么 (与 Web 版胶囊同义)
                    let gv = (!self.preset_version.trim().is_empty()).then(|| self.preset_version.trim().to_string());
                    let loader = (self.preset_loader != "auto").then(|| self.preset_loader.clone());
                    let limit = (self.preset_limit > 0).then_some(self.preset_limit);
                    if let Some(p) = crate::pipeline::preset_prefix(gv.as_deref(), loader.as_deref(), limit) {
                        ui.label(egui::RichText::new(p.trim_end()).size(11.0).color(skin::ACCENT));
                    }
                });
                ui.add_space(4.0);

                // 输入栏
                ui.horizontal(|ui| {
                    let hint = if self.chat.busy {
                        "正在处理…可点右侧\"打断\"在中途安全收尾"
                    } else {
                        "描述你的整合包需求 (Enter 发送, Shift+Enter 换行)"
                    };
                    let edit = ui.add_sized(
                        [ui.available_width() - 170.0, 76.0],
                        egui::TextEdit::multiline(&mut self.input).hint_text(hint),
                    );
                    // Enter 发送 / Shift+Enter 换行 (与 Web 版一致)
                    let enter = ui.input(|i| i.key_pressed(egui::Key::Enter) && !i.modifiers.shift);
                    ui.vertical(|ui| {
                        ui.add_space(6.0);
                        if self.chat.busy {
                            if ui.add_sized([92.0, 30.0], egui::Button::new("⏹ 打断")).clicked() {
                                self.interrupt.store(true, Ordering::Relaxed);
                                self.chat.notice("已发送打断请求, 任务将在安全点停止");
                            }
                        } else if ui.add_sized([92.0, 30.0], egui::Button::new("发送 ⏎")).clicked() {
                            self.submit();
                        }
                        if ui.add_sized([92.0, 24.0], egui::Button::new("清空对话")).clicked() {
                            self.chat.clear();
                        }
                    });
                    if enter && edit.has_focus() {
                        self.submit();
                    }
                });
                ui.add_space(6.0);
            });
    }

    // ---------------- 设置弹窗 (对应 Web 版 #settings-modal) ----------------
    fn ui_settings(&mut self, ctx: &egui::Context) {
        if !self.settings_open {
            return;
        }
        let mut open = true;
        let mut apply = false;
        egui::Window::new("设置")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .default_width(520.0)
            .show(ctx, |ui| {
                egui::Grid::new("settings-grid").num_columns(2).spacing([12.0, 8.0]).show(ui, |ui| {
                    field(ui, "API 地址", &mut self.settings.base_url);
                    ui.end_row();
                    field(ui, "API Key", &mut self.settings.api_key);
                    ui.end_row();
                    field(ui, "模型", &mut self.settings.model);
                    ui.end_row();
                    field(ui, "上下文长度", &mut self.settings.context_length);
                    ui.end_row();
                    field(ui, "输入价 ($/M tok)", &mut self.settings.price_input);
                    ui.end_row();
                    field(ui, "输出价 ($/M tok)", &mut self.settings.price_output);
                    ui.end_row();
                    field(ui, "token 预算", &mut self.settings.token_budget);
                    ui.end_row();
                    field(ui, "单轮工具上限", &mut self.settings.max_tools);
                    ui.end_row();
                    ui.label("思考参数");
                    ui.add(egui::TextEdit::singleline(&mut self.settings.thinking).hint_text("key=value, auto 或留空 = 不发送"));
                    ui.end_row();
                    ui.label("CurseForge");
                    ui.checkbox(&mut self.settings.curseforge, "启用 CurseForge 独占 mod");
                    ui.end_row();
                });
                ui.add_space(6.0);
                ui.label(egui::RichText::new("保存后立即生效, 并写回 config.toml").size(11.0).color(skin::DIM));
                if !self.settings_msg.is_empty() {
                    ui.label(egui::RichText::new(&self.settings_msg).size(11.0).color(skin::YELLOW));
                }
                ui.add_space(4.0);
                ui.horizontal(|ui| {
                    if ui.button("保存").clicked() {
                        apply = true;
                    }
                    if ui.button("取消").clicked() {
                        self.settings_open = false;
                    }
                });
            });
        if apply {
            self.apply_settings();
        }
        if !open {
            self.settings_open = false;
        }
    }

    fn apply_settings(&mut self) {
        let llm = self.settings.to_llm(&self.live_llm);
        self.settings_msg = match crate::config::save_llm(&self.config_path, &llm) {
            Ok(()) => format!("已保存, 当前模型: {}", llm.model),
            Err(e) => format!("写回 config.toml 失败: {e:#}"),
        };
        if let Err(e) = crate::config::save_curseforge(&self.config_path, self.settings.curseforge) {
            self.settings_msg.push_str(&format!(" (CurseForge 写回失败: {e:#})"));
        }
        self.model = llm.model.clone();
        self.live_llm = llm.clone();
        let _ = self.commands.send(Command::UpdateLlm(Box::new(llm)));
        let _ = self.commands.send(Command::UpdateCurseforge(self.settings.curseforge));
    }
}

impl eframe::App for DesktopApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.drain_events();
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        self.ui_topbar(ctx);
        self.ui_sidebar(ctx);
        self.ui_input(ctx);
        self.ui_chat(ctx);
        self.ui_settings(ctx);
    }
}

// ---------------------------------------------------------------------------
// 绘制小工具: 卡片、次要文字、消息行
// ---------------------------------------------------------------------------

/// 侧栏卡片: 圆角 + 边框 + 标题 (对应 CSS 的 .card)
fn card(ui: &mut egui::Ui, title: &str, body: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::none()
        .fill(skin::CARD)
        .stroke(egui::Stroke::new(1.0_f32, skin::BORDER))
        .rounding(egui::Rounding::same(14.0))
        .inner_margin(egui::Margin::same(12.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(egui::RichText::new(title).size(13.0).strong());
            ui.add_space(4.0);
            body(ui);
        });
    ui.add_space(10.0);
}

fn dim(ui: &mut egui::Ui, text: &str) {
    ui.label(egui::RichText::new(text).size(11.0).color(skin::DIM));
}

fn field(ui: &mut egui::Ui, label: &str, value: &mut String) {
    ui.label(label);
    ui.add(egui::TextEdit::singleline(value).desired_width(320.0));
}

/// 消息行 = 头像 + 气泡 (对应 .msg-row / .avatar / .msg)
fn bubble(ui: &mut egui::Ui, avatar: &str, avatar_color: egui::Color32, fill: egui::Color32, right: bool, body: impl FnOnce(&mut egui::Ui)) {
    let layout = if right {
        egui::Layout::right_to_left(egui::Align::TOP)
    } else {
        egui::Layout::left_to_right(egui::Align::TOP)
    };
    ui.with_layout(layout, |ui| {
        let avatar_label = ui.label(egui::RichText::new(avatar).size(14.0).color(avatar_color));
        let avail = ui.available_width() - 8.0;
        ui.allocate_ui_with_layout(
            egui::vec2(avail, 0.0),
            egui::Layout::top_down(egui::Align::Min),
            |ui| {
                egui::Frame::none()
                    .fill(fill)
                    .stroke(egui::Stroke::new(1.0_f32, skin::BORDER))
                    .rounding(egui::Rounding::same(14.0))
                    .inner_margin(egui::Margin::same(12.0))
                    .show(ui, |ui| {
                        ui.set_max_width(avail - 28.0);
                        body(ui);
                    });
            },
        );
        let _ = avatar_label;
    });
    ui.add_space(8.0);
}

fn draw_messages(ui: &mut egui::Ui, msgs: &[Msg]) {
    for m in msgs {
        match m {
            Msg::User(text) => bubble(ui, "我", skin::ACCENT, skin::ACCENT_SOFT, true, |ui| {
                ui.label(egui::RichText::new(text).size(13.5));
            }),
            Msg::Assistant(text) => bubble(ui, "⛏", skin::ACCENT, skin::BUBBLE, false, |ui| {
                // 桌面版先按纯文本渲染 (Web 版走 marked 渲染 markdown):
                // 代码块/列表原样保留, 不做 HTML 富文本
                ui.label(egui::RichText::new(text).size(13.5));
            }),
            Msg::Tool { name, state } => {
                let (icon, tail, color) = match state {
                    ToolState::Pending => ("⚙", "调用工具 …", skin::YELLOW),
                    ToolState::Ok => ("⚙", "完成", skin::GREEN),
                    ToolState::Fail => ("⚙", "失败", skin::RED),
                };
                ui.horizontal(|ui| {
                    ui.add_space(34.0);
                    ui.label(
                        egui::RichText::new(format!("{icon} {name} {tail}"))
                            .size(12.0)
                            .family(egui::FontFamily::Monospace)
                            .color(color),
                    );
                });
                ui.add_space(4.0);
            }
            Msg::Progress { text, current, total } => {
                ui.horizontal(|ui| {
                    ui.add_space(34.0);
                    ui.label(
                        egui::RichText::new(format!("⏳ {}{}", progress_prefix(*current, *total), text))
                            .size(12.0)
                            .family(egui::FontFamily::Monospace)
                            .color(skin::DIM),
                    );
                });
                ui.add_space(4.0);
            }
            Msg::Notice(text) => bubble(ui, "⛏", skin::DIM, skin::RAISE, false, |ui| {
                ui.label(egui::RichText::new(text).size(12.5).color(skin::DIM));
            }),
            Msg::Cards(cards) => {
                ui.horizontal(|ui| {
                    ui.add_space(34.0);
                    ui.label(egui::RichText::new("📦").size(14.0));
                    ui.label(
                        egui::RichText::new(format!("按 Web 版格式渲染的 mod 卡片 · {}", cards.len()))
                            .size(11.0)
                            .color(skin::DIM),
                    );
                });
                ui.add_space(4.0);
                let total = cards.len();
                for chunk in (0..total).step_by(2) {
                    let mut cols = ui.columns(2, |cols| {
                        for (i, col) in cols.iter_mut().enumerate() {
                            if let Some(c) = cards.get(chunk + i) {
                                mod_card(col, c);
                            }
                        }
                    });
                    let _ = &mut cols;
                    ui.add_space(6.0);
                }
            }
        }
    }
}

/// 单张 mod 卡片 (对应 .mod-card): 图标位 + 标题链接 + slug + 描述 + 下载量/分类/口味
fn mod_card(ui: &mut egui::Ui, c: &ModCard) {
    egui::Frame::none()
        .fill(skin::CARD)
        .stroke(egui::Stroke::new(1.0_f32, skin::BORDER))
        .rounding(egui::Rounding::same(12.0))
        .inner_margin(egui::Margin::same(10.0))
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                // 图标位: 桌面版不联网取图, 用圆角占位块 + 📦 (Web 版是 <img> 图标)
                egui::Frame::none()
                    .fill(skin::RAISE)
                    .rounding(egui::Rounding::same(8.0))
                    .inner_margin(egui::Margin::same(6.0))
                    .show(ui, |ui| {
                        ui.label(egui::RichText::new("📦").size(16.0));
                    });
                ui.vertical(|ui| {
                    ui.hyperlink_to(egui::RichText::new(&c.title).size(13.0).strong(), &c.url);
                    if !c.slug.is_empty() {
                        dim(ui, &c.slug);
                    }
                });
            });
            if !c.description.is_empty() {
                ui.add_space(2.0);
                let desc: String = c.description.chars().take(140).collect();
                ui.label(egui::RichText::new(desc).size(11.5).color(skin::TEXT));
            }
            let mut meta = format!("{} 下载", fmt_downloads(c.downloads));
            if !c.categories.is_empty() {
                meta.push_str(&format!(" · {}", c.categories.iter().take(4).cloned().collect::<Vec<_>>().join(", ")));
            }
            if let Some(t) = c.taste {
                meta.push_str(&format!(" · 口味 {t:.1}"));
            }
            dim(ui, &meta);
            if c.gallery > 0 {
                dim(ui, &format!("📷 {} 张截图 (桌面版暂不内嵌图片预览, 点标题可去官网看)", c.gallery));
            }
        });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_downloads_matches_web_format() {
        assert_eq!(fmt_downloads(999), "999");
        assert_eq!(fmt_downloads(5_600), "5.6K");
        assert_eq!(fmt_downloads(12_300_000), "12.3M");
    }

    #[test]
    fn progress_prefix_draws_20_cells() {
        // 3/8 → JS 的 Math.round(7.5)=8 格实心, 与 Web 版逐格一致
        let p = progress_prefix(Some(3), Some(8));
        assert_eq!(p.matches('▰').count(), 8, "{p}");
        assert_eq!(p.matches('▱').count(), 12, "{p}");
        assert!(p.contains("3/8"), "{p}");
        // 满格不越界, 缺项不画条, 总数为 0 不除零
        assert_eq!(progress_prefix(Some(99), Some(8)).matches('▰').count(), 20);
        assert_eq!(progress_prefix(Some(1), None), "");
        assert_eq!(progress_prefix(None, Some(0)), "");
    }

    #[test]
    fn extract_mods_reads_search_and_build_shapes() {
        let search = serde_json::json!({
            "query_used": "优化",
            "mods": [{
                "slug": "sodium", "title": "Sodium", "description": "渲染优化",
                "downloads": 12_345_678, "categories": ["optimization"], "url": "https://modrinth.com/mod/sodium",
                "gallery": ["a.png", "b.png"], "taste_score": 4.2
            }]
        });
        let cards = extract_mods(Some(&search));
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].title, "Sodium");
        assert_eq!(cards[0].downloads, 12_345_678);
        assert_eq!(cards[0].categories, vec!["optimization"]);
        assert_eq!(cards[0].gallery, 2);
        assert_eq!(cards[0].taste, Some(4.2));

        // 组包结果没有 title/url: 用 slug 顶替标题, 链接回落到 modrinth
        let build = serde_json::json!({ "mods": [{ "slug": "jei", "version": "1.0", "filename": "jei.jar", "size_mb": 1.2 }] });
        let cards = extract_mods(Some(&build));
        assert_eq!(cards.len(), 1);
        assert_eq!(cards[0].title, "jei");
        assert_eq!(cards[0].url, "https://modrinth.com/mod/jei");
        assert_eq!(cards[0].downloads, 0);

        // 没有 mods 字段 / 空数组都不渲染卡片
        assert!(extract_mods(Some(&serde_json::json!({ "total_hits": 0 }))).is_empty());
        assert!(extract_mods(None).is_empty());
    }

    fn hit_json() -> serde_json::Value {
        serde_json::json!({ "mods": [{
            "slug": "sodium", "title": "Sodium", "description": "渲染优化",
            "downloads": 1000, "categories": ["optimization"],
            "url": "https://modrinth.com/mod/sodium"
        }] })
    }

    /// 一轮完整工具调用: 工具行 → 进度行(原地更新) → 结果(染色+撤进度+插卡片) → 流式回复
    #[test]
    fn chat_assembles_one_tool_turn_like_web_ui() {
        let mut c = Chat::welcome();
        let head = c.messages.len(); // 欢迎语之后的位置

        c.apply(AgentEvent::ToolCall { name: "search_mods".into(), args: "{}".into() });
        assert!(c.busy);
        assert_eq!(c.messages.len(), head + 1);
        assert!(matches!(&c.messages[head], Msg::Tool { name, state: ToolState::Pending } if name == "search_mods"));

        // 同一次工具调用内的多条 progress 原地更新同一行, 不新开行
        c.apply(AgentEvent::Progress { text: "收集 sodium (1/8)".into(), current: Some(1), total: Some(8) });
        c.apply(AgentEvent::Progress { text: "收集 sodium (2/8)".into(), current: Some(2), total: Some(8) });
        assert_eq!(c.messages.len(), head + 2);
        match c.messages.last() {
            Some(Msg::Progress { text, current, total }) => {
                assert_eq!(text, "收集 sodium (2/8)");
                assert_eq!((*current, *total), (Some(2), Some(8)));
            }
            Some(_) => panic!("最后一条不是进度行"),
            None => panic!("进度行丢了"),
        }

        // 工具结果: 工具行转 ok + 进度行撤掉 + mod 卡片插入 (只剩 工具行 + 卡片)
        c.apply(AgentEvent::ToolResult {
            name: "search_mods".into(),
            ok: true,
            result: Some(hit_json()),
        });
        assert_eq!(c.messages.len(), head + 2);
        assert!(matches!(&c.messages[head], Msg::Tool { state: ToolState::Ok, .. }));
        assert!(matches!(&c.messages[head + 1], Msg::Cards(v) if v.len() == 1 && v[0].title == "Sodium"));

        // 流式增量拼进同一个气泡, Reply 用完整文本覆盖而不是新增气泡
        c.apply(AgentEvent::ReplyDelta { text: "找到".into() });
        c.apply(AgentEvent::ReplyDelta { text: "了 Sodium".into() });
        assert_eq!(c.messages.len(), head + 3);
        c.apply(AgentEvent::Reply { text: "找到了 Sodium".into() });
        assert_eq!(c.messages.len(), head + 3);
        assert!(matches!(c.messages.last(), Some(Msg::Assistant(t)) if t == "找到了 Sodium"));

        assert!(!c.busy);
        assert_eq!(c.status(), "就绪");
        // 一轮结束要让侧栏重扫 (整合包列表/口味库), 且只上报一次
        assert!(c.take_dirty());
        assert!(!c.take_dirty());
    }

    /// 工具行之后的文本必须另起气泡, 否则回复会续写到工具行上方的旧气泡里
    #[test]
    fn reply_after_tool_call_starts_new_bubble() {
        let mut c = Chat::welcome();
        c.apply(AgentEvent::ReplyDelta { text: "先看看".into() });
        let first = c.messages.len() - 1;
        c.apply(AgentEvent::ToolCall { name: "build_modpack".into(), args: "{}".into() });
        c.apply(AgentEvent::ToolResult { name: "build_modpack".into(), ok: true, result: Some(serde_json::json!({ "mods": [] })) });
        c.apply(AgentEvent::ReplyDelta { text: "打包好了".into() });
        assert_eq!(c.messages.len(), first + 3); // 旧气泡 + 工具行 + 新气泡
        assert!(matches!(&c.messages[first], Msg::Assistant(t) if t == "先看看"));
        assert!(matches!(c.messages.last(), Some(Msg::Assistant(t)) if t == "打包好了"));
    }

    /// 推理片段只喂状态栏, 不进对话; 工具失败行染色; token 增量累加到快照前
    #[test]
    fn reasoning_stays_out_of_transcript_and_failures_are_marked() {
        let mut c = Chat::welcome();
        // 真实时序: 一轮已经开跑 (busy), 此时收到的思考片段只更新状态栏
        c.apply(AgentEvent::ToolCall { name: "search_mods".into(), args: "{}".into() });
        let head = c.messages.len();
        c.apply(AgentEvent::ReasoningDelta { text: "用户在问版本".into() });
        assert_eq!(c.messages.len(), head);
        assert!(c.status().starts_with("思考中…"), "{}", c.status());

        // 工具失败: 工具行染色, 且不进卡片
        c.apply(AgentEvent::ToolResult { name: "search_mods".into(), ok: false, result: None });
        assert!(matches!(c.messages.last(), Some(Msg::Tool { state: ToolState::Fail, .. })));

        // 快照到达前, 本轮 token 增量也要能在统计卡片里动
        c.apply(AgentEvent::LlmUsage { prompt_tokens: 10, completion_tokens: 5, total_tokens: 15 });
        c.apply(AgentEvent::LlmUsage { prompt_tokens: 1, completion_tokens: 1, total_tokens: 2 });
        assert_eq!(c.token_delta, 17);

        c.apply(AgentEvent::Reply { text: "好的".into() });
        assert_eq!(c.status(), "就绪");
    }

    /// 无窗口冒烟: 装皮肤+中文字体, 再把所有消息种类都画一遍 (含 mod 卡片网格)。
    /// egui 的排版是纯 CPU 的, 所以这条测试能真跑一遍渲染路径 ——
    /// 字体装不上/布局算出非法尺寸之类的问题会在这里炸, 而不是等用户开窗口才发现。
    #[test]
    fn skin_and_all_message_kinds_render_headless() {
        let ctx = egui::Context::default();
        install_skin(&ctx);
        let font = install_fonts(&ctx);
        #[cfg(windows)]
        assert!(font.is_some(), "Windows 上应当能找到系统里的中文字体");

        let mut c = Chat::welcome();
        c.apply(AgentEvent::ToolCall { name: "search_mods".into(), args: "{}".into() });
        c.apply(AgentEvent::Progress { text: "收集 sodium (2/8)".into(), current: Some(2), total: Some(8) });
        c.apply(AgentEvent::ToolResult {
            name: "search_mods".into(),
            ok: true,
            result: Some(hit_json()),
        });
        c.apply(AgentEvent::ReplyDelta { text: "找到 1 个候选: Sodium".into() });
        c.apply(AgentEvent::Reply { text: "找到 1 个候选: Sodium".into() });
        c.apply(AgentEvent::ToolCall { name: "build_modpack".into(), args: "{}".into() });
        c.apply(AgentEvent::ToolResult { name: "build_modpack".into(), ok: false, result: None });
        c.notice("已发送打断请求, 任务将在安全点停止");

        // 一帧 = 顶栏之外的全部绘制函数: 气泡/工具行/卡片网格/侧栏卡片
        let output = ctx.run(egui::RawInput::default(), |ctx| {
            egui::SidePanel::left("sidebar-test").show(ctx, |ui| {
                card(ui, "统计", |ui| {
                    ui.label("调用 2 次");
                    dim(ui, "尚无调用");
                });
            });
            egui::CentralPanel::default().show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| draw_messages(ui, &c.messages));
            });
        });
        assert!(!output.shapes.is_empty(), "这一帧什么都没画出来");
        assert!(matches!(c.messages.last(), Some(Msg::Notice(_))));
    }
}
