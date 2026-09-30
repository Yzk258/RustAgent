//! 原生桌面 UI。与 Web UI 并存，`cargo run -- desktop` 启动。
//! 窗口内直接消费 AgentEvent，不启动 HTTP 服务或系统浏览器。

use crate::agent::{new_agent, AgentEvent};
use crate::config::Config;
use crate::prelude::*;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread;

enum Command {
    Chat(String),
}

pub fn run(cfg: Config) -> Result<()> {
    let download_dir = std::path::absolute(&cfg.output.download_dir)
        .unwrap_or_else(|_| std::path::PathBuf::from(&cfg.output.download_dir))
        .display().to_string();
    let (cmd_tx, cmd_rx) = mpsc::channel::<Command>();
    let (event_tx, event_rx) = mpsc::channel::<AgentEvent>();
    let interrupt = Arc::new(AtomicBool::new(false));
    let worker_interrupt = interrupt.clone();
    thread::spawn(move || {
        let runtime = match tokio::runtime::Runtime::new() {
            Ok(rt) => rt,
            Err(e) => {
                let _ = event_tx.send(AgentEvent::Reply { text: format!("启动失败: {e:#}") });
                return;
            }
        };
        runtime.block_on(async move {
            let mut agent = match new_agent(&cfg, worker_interrupt).await {
                Ok(agent) => agent,
                Err(e) => {
                    let _ = event_tx.send(AgentEvent::Reply { text: format!("初始化 Agent 失败: {e:#}") });
                    return;
                }
            };
            while let Ok(Command::Chat(input)) = cmd_rx.recv() {
                let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
                let forward = event_tx.clone();
                let forwarder = tokio::spawn(async move {
                    while let Some(event) = rx.recv().await {
                        let _ = forward.send(event);
                    }
                });
                if let Err(e) = agent.run_turn_with(&input, &tx).await {
                    let _ = event_tx.send(AgentEvent::Reply { text: format!("执行失败: {e:#}") });
                }
                drop(tx);
                let _ = forwarder.await;
            }
        });
    });

    eframe::run_native(
        "RustAgent",
        eframe::NativeOptions::default(),
        Box::new(move |_cc| Ok(Box::new(DesktopApp::new(cmd_tx, event_rx, interrupt, download_dir)))),
    ).map_err(|e| anyhow!("桌面窗口启动失败: {e}"))
}

struct DesktopApp {
    commands: Sender<Command>,
    events: Receiver<AgentEvent>,
    interrupt: Arc<AtomicBool>,
    input: String,
    transcript: String,
    busy: bool,
    status: String,
    download_dir: String,
}

impl DesktopApp {
    fn new(commands: Sender<Command>, events: Receiver<AgentEvent>, interrupt: Arc<AtomicBool>, download_dir: String) -> Self {
        Self { commands, events, interrupt, input: String::new(), transcript: "欢迎使用 RustAgent。\n告诉我你的 Minecraft 版本、加载器和想要的 mod，我会帮你搜索并生成整合包。\n".into(), busy: false, status: "就绪".into(), download_dir }
    }

    fn submit(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() || self.busy { return; }
        self.transcript.push_str(&format!("\n你：{text}\n\n助手："));
        self.input.clear();
        self.busy = true;
        self.status = "正在处理…".into();
        let _ = self.commands.send(Command::Chat(text));
    }
}

impl eframe::App for DesktopApp {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                AgentEvent::ReplyDelta { text } => self.transcript.push_str(&text),
                AgentEvent::Reply { text } => { if !self.transcript.ends_with(&text) { self.transcript.push_str(&text); } self.transcript.push_str("\n\n"); self.busy = false; self.status = "就绪".into(); }
                AgentEvent::ToolCall { name, .. } => { self.status = format!("正在调用 {name}"); self.transcript.push_str(&format!("\n[工具] {name}\n")); }
                AgentEvent::ToolResult { name, ok, .. } => self.transcript.push_str(&format!("[{name}: {}]\n", if ok { "完成" } else { "失败" })),
                AgentEvent::Progress { text, .. } => { self.status = text.clone(); self.transcript.push_str(&format!("\n[进度] {text}")); }
                AgentEvent::ReasoningDelta { .. } | AgentEvent::LlmUsage { .. } => {}
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        eframe::egui::CentralPanel::default().show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("RustAgent");
                ui.label("原生桌面模式");
                ui.with_layout(eframe::egui::Layout::right_to_left(eframe::egui::Align::Center), |ui| {
                    if ui.button("打开下载目录").clicked() {
                        #[cfg(windows)] let _ = std::process::Command::new("explorer.exe").arg(&self.download_dir).spawn();
                    }
                    if ui.button("清空记录").clicked() { self.transcript.clear(); }
                });
            });
            ui.label(format!("下载目录：{}", self.download_dir));
            ui.separator();
            eframe::egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| { ui.label(&self.transcript); });
            ui.separator();
            ui.label(&self.status);
            ui.horizontal(|ui| {
                let response = ui.add(eframe::egui::TextEdit::multiline(&mut self.input).desired_rows(3).hint_text("输入需求，例如：帮我找 1.21.1 Fabric 的生存整合包"));
                if response.lost_focus() && ui.input(|i| i.key_pressed(eframe::egui::Key::Enter)) { self.submit(); }
                if ui.button("发送").clicked() { self.submit(); }
                if ui.button("停止").clicked() { self.interrupt.store(true, Ordering::Relaxed); }
            });
        });
    }
}
