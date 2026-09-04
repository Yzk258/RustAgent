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
- 会话自动保存已实现（默认总是保存每一次对话，CLI 与 Web 同步）：Agent 新增 session_file 字段（当前自动保存目标，None=未保存过）；history.rs 新增 auto_save(agent)——首轮对话时生成 sessions/auto-{ts}.json，此后同一会话固定覆盖写同一文件（ts=首轮时间），无 user 消息时跳过；原 save 保留为手动另存快照（session-{ts}.json）。接入点两处：CLI main.rs 每轮 select! 收尾后自动保存并 dim 打印路径；Web api.rs chat 持锁块内 run_turn_with 之后 auto_save，done 事件新增 saved 字段（空串=未保存）。前端同步：会话卡片新增"自动保存: <路径>"行（数据来自 /api/info 新增的 session_file 字段，尚无对话显示"尚无对话"），handleEvent 的 done 分支收到 saved 非空即刷新会话列表（finally 里 refreshSidebar 本就会刷，双保险）。load_path 加载/导入后重置 session_file=None，继续对话 fork 出新文件不覆盖被导入的历史。冒烟验证：cargo build 无警告（clippy 4 条均为存量），ui 服务 health/info/sessions 200，info.session_file 字段生效。
- 长任务"超 3 秒必有实时反馈 + 可打断"通用机制已落地（对照需求：如照片处理显示"已处理 45/120 张"、数学证明显示"正在尝试证明引理…"）：
  - 进展结构化：tools.rs 的 ProgressTx 从 String 升级为 ProgressUpdate{text, current, total}（数值进度驱动两端进度条），新增 TaskCtx{progress, interrupt} 长任务上下文，execute 第三参数从 Option<&ProgressTx> 改为 &TaskCtx（repair 子命令/selftest 传 TaskCtx::none()）；约定超 3s 的工具必须接 ctx 并 report 进展、循环间隙 check_interrupt。
  - 3s 静默兜底（agent.rs）：每次工具执行 spawn watchdog（500ms tick），运行超 3s 且距上次进展超 3s 即发"xx 仍在执行… (Ns)"，真实进展刷新 last_activity 原子不干扰，工具结束 abort。任何工具（含未来新增、忘接进展的）都被兜底覆盖。
  - 工具内打断：build_modpack / repair_pack / dependency_closure 的每个 mod 收集循环开头 check_interrupt()，打断请求秒级生效（此前只能在工具间隙响应，组包 100 mod 要等全部跑完）；错误串含 TOOL_INTERRUPTED 标记，agent 识别后推占位 tool 消息直接 abort_turn 收尾，不再回传 LLM 浪费 token。
  - CLI 进度条：cli.rs 新增 ProgressPrinter（零依赖 ANSI，▰▱ 条 + 单行原地刷新 \r\x1b[2K，工具行/回复打印前先清进展行防错位）；不用 indicatif 的原因——REPL Ctrl+C 直接 abort 打印任务，自绘行只是停在原地，三方库全局 draw target 会残留持续抢占 stdout。demo 流水线同步适配新通道。
  - Web：progress 事件带 current/total（None→null），app.js progressPrefix 渲染 ▰▱ 条；NDJSON 流式与 SSE 同类（单向实时推送），前端 fetch 解析已稳定，不换协议。
  - 验证：cargo build 通过、clippy 4 条均为存量；cargo run -- selftest 走真实 Modrinth API 组包通过（新签名无回归）；ui 服务 health 200 + app.js 含进度条渲染代码。
- 卡死 bug 修复（上线即踩坑）：进展通道死锁——agent.rs 里 ptx clone 进 TaskCtx 后又单独 drop(ptx)，ctx 还持有发送端导致通道永不关闭，工具执行结束 `forwarder.await` 死等，整轮对话卡在"思考中"（CLI/Web 均卡）。修复：ptx 直接 move 进 ctx（不留多余 clone），工具结束后 drop(ctx) 作为唯一发送端关闭通道。实测"搜索 sodium"走完 2 次 LLM 调用 + 工具结果回传 + 回复 + 自动保存，不再卡死。教训：mpsc 通道关闭语义看"全部发送端"，包装进上下文结构后必须审视谁还握着 clone。
- LLM 空窗期实时反馈补齐（用户反馈"思考中不显示进度"的真正痛点：进展事件只覆盖工具执行那几秒，而一轮对话的大头是 LLM 调用 10~30s，期间只有静态"思考中"）：agent.rs 每次 LLM 调用 spawn LLM watchdog（500ms tick，超 3s 周期发"模型思考中… (Ns)"进展事件），select! 的成功/失败/打断三条退出路径都 abort 防泄漏；前端 showThinking 内置 setInterval 每秒更新"思考中 (Ns)"（hideThinking 清计时器），3s 后 agent 侧进展行接管。CLI 端 ProgressPrinter 自动单行刷新。实测组包全流程日志：模型思考中 (3s→5s) → 解析依赖闭包 → ▰▰▰▱ 1/2 正在收集 mod sodium → 2/2 fabric-api → 写入 → 回复，CLI/Web 两端全程有秒级反馈。
- LLM 改流式输出（治本"模型思考中… (73s)"）：此前非流式 = 整个响应生成完才返回，长回复期间只能看秒数跳。llm.rs 新增 chat_stream：SSE 逐行解析（bytes_stream + 手动按 \n 分帧，data: [DONE] 结束），delta.content 增量经 on_delta 回调即时转成 AgentEvent::ReplyDelta（打字机效果，首个 token 1~3s 可见），tool_calls 分片按 index 拼装（id/name upsert + arguments 追加），usage 经 stream_options.include_usage 获取（缺失则 default），响应非 text/event-stream 时回退一次性 JSON 解析；服务端不支持流式也不炸。reqwest 加 "stream" feature，流解析用 tokio_stream::StreamExt（不加 futures-util）。事件链路：ReplyDelta → CLI printer.delta() 逐片段 write（首个片段前空行，Reply 到达只换行收尾）；Web reply_delta 事件追加纯文本到气泡、完整 reply 到达后 renderText 重渲染（markdown 生效）。修了一个前端致命坑：send() 里 thinking 变量重复 const 声明（会让整个 app.js SyntaxError），合并为单声明。chat() 非流式方法已无人用，删除。实测组包全流程（3 次 LLM 流式调用 + 工具 + 组包 + 保存）无卡死，node --check 通过，ui 冒烟 200。

## 2026-09-03：可靠性优化

- 完成配置启动校验：检查 URL 协议、上下文长度、token 单价、工具调用轮数和下载目录，避免运行很久后才暴露配置错误。
- 完成会话与用户数据库的临时文件写入：保存内容先落到 `*.tmp-进程号`，再替换正式 JSON；目录创建失败现在会返回错误，不再被忽略。
- 修复工具错误 JSON 的构造方式：改用 `serde_json::json!`，避免错误文本中的引号或换行破坏 JSON。
- 收紧 Minecraft 版本格式校验，拒绝空段版本号，例如 `1..21`。
- 新增 4 个自动测试，覆盖 LLM tool call 分片合并、预设过滤、版本/加载器校验、用户口味权重排序。
- 验证结果：`cargo test` 为 4 passed；`cargo clippy --all-targets -- -D warnings` 通过；已执行 `cargo fmt`。

本次没有处理 API Key 暴露问题，按本轮需求保留为单独事项。

## 2026-09-03：前端结构优化

- `src/ui/static/app.js` 增加 `DOM` 节点缓存和集中式 `state`，减少散落的 `getElementById` 与全局变量依赖。
- API 请求统一由 `API.request` 处理响应文本、JSON 解析和 HTTP 错误；侧栏四个数据面板用 `Promise.allSettled` 并行刷新，互不阻塞。
- 聊天输入改为表单提交，同时保留 Enter 发送和 Shift+Enter 换行；预设区域阻止误提交。
- `index.html` 增加 `role=log`、`aria-live` 和表单语义，聊天内容对辅助工具更友好。
- `style.css` 增加预设换行、移动端间距、气泡宽度和滚动槽规则，改善手机窄屏布局。
- 验证：`node --check src/ui/static/app.js` 通过；`cargo test` 4 个测试全部通过；`git diff --check` 无错误。

## 2026-09-05：README 用户文档同步

- 启动一节补充 CLI 流式回复输出、组包 ▰▱ 进度条、3s 兜底提示与 Ctrl+C 打断说明。
- 项目结构补上 `src/cli.rs`；agent/llm/database/history/config 的职责描述同步最新实现（流式、SQLite、自动保存、启动校验）。
- 纯文档改动，无代码变更。

## 待办 / TODO

- [ ] 优化根据prompt随机找包机制（固定公式->带有一定随机分布避免重复）
- [ ] "试试这个" 定期推荐窗口
- [ ] Web 界面（已落地，后续考虑扩展至pcl2启动器）
- [ ] token 显示与预算上限自动中断机制
- [ ] 冲突检测基于元数据的更细粒度报告
- [ ] 用户口味数据库的反馈闭环完善

## 2026-09-03：用户数据库升级为 SQLite

- 原来是一个 JSON 文件里的两个数组，数据增长后每次保存都要整体读写，反馈标签和整合包模组也没有独立关系。
- 现在拆成 `feedback`、`feedback_tags`、`packs`、`pack_mods` 四张 SQLite 表，并增加外键、级联删除和常用索引。
- 保存使用事务，反馈、标签、整合包和模组关系不会只写入一部分。
- 继续兼容 `database.path = "./userdata.json"`：实际使用同目录 `userdata.db`；第一次启动自动导入旧 JSON，旧文件不删除。
- 上层推荐、反馈和 Web 接口继续调用 `UserDatabase`，本次没有改变用户操作方式。
- 验证：SQLite 关系往返测试通过，`cargo test` 4 个通过，严格 Clippy 通过。

## 2026-09-03：移除开发期旧数据库兼容层

- 项目仍在开发阶段，不再支持从 `userdata.json` 自动迁移；配置默认数据库改为 `userdata.db`。
- 删除了 `.json -> .db` 路径转换、旧 JSON 反序列化结构和保存时多余的 path 参数。
- `modrinth-test/` 暂时保留：它是独立的真实 Modrinth API/整合包验证程序，不属于旧数据库兼容接口。

## 2026-09-03：tools.rs 报红修复

- `build_modpack` 和 `repair_pack` 原先在版本返回空 `files` 时使用 `v.files[0]`，存在越界崩溃风险。
- 改为显式处理空文件列表：记录冲突并继续其他 mod；正常版本仍优先使用 primary 文件。
- 删除 `VersionFile` 无用导入，解决严格检查下的报红。
- 验证：`cargo test` 4 个通过，`cargo clippy --all-targets -- -D warnings` 通过，`git diff --check` 通过。
- [ ] 组完包后自动化拖入pcl2启动器纠错的可选机制补充
- [ ] 组包mod数量可选择化（1-10、10-20...）、mod条件可选择化（从最新或是最热两种模式找mod）→ 找包数量已可选(5-20+自定义, 单次上限20, 单包上限100), mod来源排序(最新/最热)待做
- [X] Web UI 增强：聊天中途打断按钮、整合包点击下载/打开目录、会话历史列表展示
- [X] Web UI 实时进展显示（组包阶段"正在收集 mod x (i/n)"已可见，其他工具可按需接入 ProgressTx）

## 2026-09-03：修正 PackRecord 序列化报红

- `/api/profile` 会把 `UserDatabase.packs` 返回给前端，因此 `PackRecord` 必须实现 `serde::Serialize`；当前定义已补齐该派生。
- 增加数据库测试断言，确保整合包记录可以转换为 JSON，避免 Rust Analyzer 或编译检查再次出现同类问题。
- 更正前一条 SQLite 记录：开发阶段已移除旧 `userdata.json` 迁移和兼容逻辑，当前只使用配置指定的 SQLite 数据库路径。
