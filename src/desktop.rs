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
        Box::new(move |_cc| Ok(Box::new(DesktopApp::new(cmd_tx, event_rx, interrupt)))),
    ).map_err(|e| anyhow!("桌面窗口启动失败: {e}"))
}

struct DesktopApp {
    commands: Sender<Command>,
    events: Receiver<AgentEvent>,
    interrupt: Arc<AtomicBool>,
    input: String,
    transcript: String,
    busy: bool,
}

impl DesktopApp {
    fn new(commands: Sender<Command>, events: Receiver<AgentEvent>, interrupt: Arc<AtomicBool>) -> Self {
        Self { commands, events, interrupt, input: String::new(), transcript: String::new(), busy: false }
    }

    fn submit(&mut self) {
        let text = self.input.trim().to_string();
        if text.is_empty() || self.busy { return; }
        self.transcript.push_str(&format!("\n你：{text}\n\n助手："));
        self.input.clear();
        self.busy = true;
        let _ = self.commands.send(Command::Chat(text));
    }
}

impl eframe::App for DesktopApp {
    fn update(&mut self, ctx: &eframe::egui::Context, _frame: &mut eframe::Frame) {
        while let Ok(event) = self.events.try_recv() {
            match event {
                AgentEvent::ReplyDelta { text } => self.transcript.push_str(&text),
                AgentEvent::Reply { text } => { if !self.transcript.ends_with(&text) { self.transcript.push_str(&text); } self.transcript.push_str("\n"); self.busy = false; }
                AgentEvent::ToolCall { name, .. } => self.transcript.push_str(&format!("\n[工具] {name}\n")),
                AgentEvent::ToolResult { name, ok, .. } => self.transcript.push_str(&format!("[{name}: {}]\n", if ok { "完成" } else { "失败" })),
                AgentEvent::Progress { text, .. } => self.transcript.push_str(&format!("\n[进度] {text}")),
                AgentEvent::ReasoningDelta { .. } | AgentEvent::LlmUsage { .. } => {}
            }
        }
        ctx.request_repaint_after(std::time::Duration::from_millis(50));
        eframe::egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("RustAgent");
            ui.separator();
            eframe::egui::ScrollArea::vertical().stick_to_bottom(true).show(ui, |ui| { ui.label(&self.transcript); });
            ui.separator();
            ui.horizontal(|ui| {
                let response = ui.text_edit_singleline(&mut self.input);
                if response.lost_focus() && ui.input(|i| i.key_pressed(eframe::egui::Key::Enter)) { self.submit(); }
                if ui.button("发送").clicked() { self.submit(); }
                if ui.button("停止").clicked() { self.interrupt.store(true, Ordering::Relaxed); }
            });
        });
    }
}
