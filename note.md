# RustAgent 工作笔记

> 本文件作为日常工作目录/索引，记录项目现状、结构与待办。后续工作以此为准。

## 项目概述

- 目标：智能化管理 Minecraft 模组的 Agent（选 mod → 生成整合包 → 兼容性检测 → 用户口味数据库 → "试试这个"推荐）。
- 详细背景见 `README.md`、`Background.md`、`Intro.md`。
- 核心逻辑 Rust 实现，功能以 skill/tool 形式搭载在不同 LLM 上（OpenAI 兼容远程 / 本地 Ollama，经 `config.toml` 切换）。

## 技术栈

- Rust 2021，tokio（多线程 + io-std）、reqwest(json)、serde/serde_json、toml、zip、chrono、anyhow。

## 代码结构（src/）

| 文件        | 职责                                                   |
| ----------- | ------------------------------------------------------ |
| main.rs     | 入口：CLI REPL、子命令分发（selftest / demo / repair） |
| agent.rs    | Agent 循环：LLM 对话 + 工具调用编排                    |
| llm.rs      | LLM 客户端（OpenAI 兼容 / Ollama），token 用量统计     |
| config.rs   | config.toml 加载与模型配置                             |
| modrinth.rs | Modrinth API 客户端（搜索/详情/版本/依赖）             |
| tools.rs    | ToolRegistry：搜索、下载、兼容检查、repair_pack 等     |
| pipeline.rs | demo 流水线（run_demo）                                |
| database.rs | 用户口味数据库（userdata.json）                        |
| history.rs  | 会话历史保存/加载（JSON）                              |
| ui/         | Web 界面：axum + REST/NDJSON 流式 API + 内嵌前端三件套 |

## 配置与数据

- `config.toml`（实际使用）/ `config.example.toml`（模板）：API Endpoint、Key、模型、下载目录等。
- `userdata.json`：用户口味数据。
- `modrinth-test/`：测试下载目录；`downloads/`：输出下载目录。

## 常用命令

```powershell
cargo build                     # 编译
cargo run                       # CLI 交互模式
cargo run -- selftest           # 自检
cargo run -- demo [需求描述]     # 演示流水线
cargo run -- repair <pack文件>   # 修复整合包
cargo fmt / cargo clippy        # 格式化 / 静态检查
```

## 当前进度

- Git 最新提交：`demo测试版`（v0.2 测试版）。
- CLI REPL 已有：/new /save /load /stats /quit，Ctrl+C 打断任务不退出。
- 已实现：Modrinth 检索与下载、兼容性检查、repair_pack、demo 流水线、token 统计、历史管理。
- Web UI 已落地（cargo run -- ui，默认端口 1780）：
  - 后端 src/ui/mod.rs + api.rs：静态三件套编译期内嵌，/api/* 全套接口，聊天为 NDJSON 流式事件（tool_call/tool_result/progress/reply/error/done），Agent 互斥锁串行化；
  - 前端 src/ui/static/{index.html, style.css, app.js}：聊天流式渲染（工具调用状态行）、侧栏统计/口味权重/工具清单/整合包列表、会话管理、主题 CSS 变量化；
  - 冒烟测试通过：health/info/tools/packs/静态文件均 200；修复了 mod.rs 静态路由漏 .await 的编译错误。
- 输入框消失 bug 彻底修复（第二版）：首次只加 #messages min-height:0 不够 —— grid 行的裸 1fr 实为 minmax(auto,1fr) 仍会被内容撑破整页。现四层加固：#app 行用 minmax(0,1fr) + overflow hidden、#chat min-height:0 + overflow hidden、#messages min-height:0 + 内部滚动、#inputbar flex-shrink:0；静态响应加 Cache-Control: no-cache 防浏览器用旧 CSS（改前端后重启服务即生效）。已验证服务端下发内容含全部修复。
- 实时进展显示已实现：AgentEvent 新增 Progress 事件；tools.rs 的 execute 接 ProgressTx 通道，build_modpack/repair_pack 在闭包解析/收集 mod ("正在收集 mod x (i/n)")/写文件阶段上报；api.rs 转发为 NDJSON progress 事件；app.js 用单行原地刷新渲染（蓝色 + 动画点点），CLI 终端与 demo 流程同样实时打印 ⏳ 进展。demo 实测通过。
- "思考中"加载提示已实现（纯前端，无协议改动）：发出消息后与每个工具结果返回后的 LLM 空窗期显示灰色"思考中…"动画行，tool_call/reply/error 事件到达即消失，用户不会再担心卡死。
- 预设选项栏已实现（省一轮追问 API 调用）：输入区上方固定 MC 版本（datalist 常用版本+可自由输入）、加载器、找包数量选择，localStorage 记忆；随 /api/chat 请求体可选字段上送，api.rs::apply_preset 校验后以 "[界面预设: ...]" 注入用户消息，系统提示词已声明预设视为确认、绝不再追问。无效值静默忽略不阻塞对话。
- 预设选项栏 422 bug 修复：找包数量经 <select></select> 取值是字符串 "8"，而后端 ChatRequest 声明 Option<u32></u32>，serde 反序列化失败被 axum 提取器整体拒绝 (422)。双层修复：前端发送前 parseInt 转数字；后端 search_limit 改为 Option[serde_json::Value](serde_json::Value) + parse_limit() 宽容解析（数字/字符串均可、非法静默忽略），此类预设字段从此不会再引发 422。app.js 非 200 错误现在会附带响应体片段便于排查。实测字符串形态 200、畸形 JSON 400。
- 工具调用上限改为可配置：config.toml [llm] max_tool_iterations（默认 16，原固定 8），复杂组包需求不再轻易报"超过上限"。
- 找包数量选择已加入预设栏（5/8/10/15/20）：随请求上送 search_limit，agent 作为 search_mods 的 limit 参数使用，玩家可自由选择多找/少找。
- 找包数量新增"自定义"档：选中后出现数字输入框 + 红色"上限 20"提醒；前后端双重钳制单次对话上限 20（app.js currentLimit / api.rs parse_limit 1..=20）。
- 单包 mod 上限 100 已实现：tools.rs 常量 MAX_USER_MODS_PER_PACK，build_modpack 用户所选超限即中止并说明"为考虑轻量化敬请谅解"；依赖自动补全与报错修复补入（repair_pack）不计入；系统提示词要求 agent 主动向用户解释并建议分多轮组包。
- config.toml 已显式写入 max_tool_iterations = 32（原默认 16），配合更大量的单轮需求。
- 会话记录功能重构（修复"加载点了没反应"+ 会话导入）：根因是 /api/session/load 只把会话灌回 agent 上下文而前端从不渲染。现在 load 响应附带可渲染消息并渲染；会话卡新增记录列表（/api/sessions，逐文件校验合法性，非法置灰）与"打开目录"按钮（/api/session/open，可手动放入合法 session json 导入）；点击列表条目 /api/session/import 按文件名导入（拒绝路径穿越、serde 校验合法性、失败不污染当前会话）并渲染整个对话（user/assistant 气泡 + 工具调用行）。
- 打断按钮已实现（协作式取消）：Agent 持 Arc<AtomicBool></atomicbool> interrupt 标记；POST /api/chat/interrupt 置位（不锁 agent）；LLM 调用用 tokio::select! + 200ms 轮询即时打断（reqwest 请求随 future 取消，不耗 token）；工具调用在间隙逐个检查。收尾时给未执行的工具调用补占位 tool 消息保证消息序列合法，并发 Reply"⏸ 已打断当前任务"。前端 busy 时输入区显示红色 ⏸ 打断按钮。实测：对话 4 秒后打断，流内正确收到打断回复，0 token 消耗。
- 版本号统一：Cargo.toml [package] version 为单一来源（当前 0.2.0），CLI 横幅改为 env!("CARGO_PKG_VERSION") 读取（原写死 v0.2），API 与前端顶栏原本就读它。更新版本只改 Cargo.toml 一处。
- CLI 会话预设 /set 已实现（回答"CLI 是否无法改包数"）：/set 版本=1.21.1 加载器=fabric 数量=10，与 Web 预设栏共用 pipeline::preset_prefix 注入逻辑（UI 的 apply_preset 重构为调用它，去重）；键名中英文别名、数量钳制 1..=20、非法值拒绝不覆盖旧值；预设仅存 REPL 内存。CLI 一直读取 config.toml（工具上限/token 预算均生效），此前缺的只是预设入口。
- CLI 美化重构（仿 opencode/claude code 风格，新增 src/cli.rs 零依赖 ANSI 模块）：圆角边框横幅（accent 亮青边框、加粗标题含版本号、dim 副标题含模型名；CJK 按 2 列计宽保证右边框对齐，程序化校验四行等宽）；`❯` 提示符带 dim 预设标签 [1.21.1·fabric·×10]；工具事件彩色状态行（⚙ 青调用/✓ 绿完成/✗ 红失败/⏳ 黄进展，ToolResult 此前 CLI 不打印现在补上）；/set 用法 dim、错误红 ✗、成功绿 ✓、打断黄 ⏸；会话结束 dim 分隔线；Windows 启动时 kernel32 开启 VT 处理支持颜色。
- 宽度计算换用 unicode-width crate（0.2.2，East Asian Width 权威实现，替代手写范围表，正确处理组合字符/CJK 扩展区）；同时把模糊宽度的 ⛏ 从横幅标题移到帮助行——模糊字符在不同终端渲染宽度不一致会破坏边框对齐。横幅四行等宽校验通过。
- "打开目录"按钮已加入整合包卡片：POST /api/packs/open 在资源管理器中打开输出目录（不存在则先创建），实测弹出正常。
- 打开目录 bug 修复：config 的 download_dir 是相对路径 (./downloads)，explorer 不解析相对路径导致打开的是默认位置。现 spawn 前用 std::path::absolute 转绝对路径（基准=进程工作目录，与包写入位置一致），返回消息也改为显示解析后的绝对路径便于核对。

## 待办 / TODO

- [ ] 优化根据prompt随机找包机制（固定公式->带有一定随机分布避免重复）
- [ ] "试试这个" 定期推荐窗口
- [ ] Web 界面（已落地，后续考虑扩展至pcl2启动器）
- [ ] token 显示与预算上限自动中断机制
- [ ] 冲突检测基于元数据的更细粒度报告
- [ ] 用户口味数据库的反馈闭环完善
- [ ] 组完包后自动化拖入pcl2启动器纠错的可选机制补充
- [ ] 组包mod数量可选择化（1-10、10-20...）、mod条件可选择化（从最新或是最热两种模式找mod）→ 找包数量已可选(5-20+自定义, 单次上限20, 单包上限100), mod来源排序(最新/最热)待做
- [X] Web UI 增强：聊天中途打断按钮、整合包点击下载/打开目录、会话历史列表展示
- [X] Web UI 实时进展显示（组包阶段"正在收集 mod x (i/n)"已可见，其他工具可按需接入 ProgressTx）
