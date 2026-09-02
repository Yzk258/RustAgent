# RustAgent

一款用 Rust 编写的 **Minecraft 模组管理 Agent**：你用自然语言描述想要什么（版本、加载器、玩法偏好），它替你去 Modrinth 搜索、筛选、组包，最后生成一个可以直接拖进启动器（如 PCL2）安装的 `.mrpack` 整合包文件。

项目背景与设计初衷见 [Background.md](Background.md)。

## 它解决什么问题

- **找 mod 靠大海捞针**：现有整合包玩腻了，不知道还有什么好玩的 mod。RustAgent 根据你的口味描述自动搜索并给出候选清单，你只负责挑选。
- **组包兼容性地狱**：手动一个个检查版本、加载器、前置依赖非常低效。RustAgent 生成整合包时自动解析每个 mod 的依赖闭包，并基于元数据检测潜在冲突后汇报。
- **越用越懂你**：你对 mod 的喜欢/不喜欢会记入用户数据库，之后每次推荐都会按你的口味排序，还会主动"试试这个"推荐你没见过的新 mod。

## 核心功能

1. **智能搜索与组包**：描述需求（如"1.21.1 fabric 生存整合包，带点探索和装饰"），agent 调用 Modrinth API 搜索真实存在的 mod（绝不凭记忆瞎编），列出候选供你筛选，确认后自动补全前置依赖并生成 `.mrpack`。
2. **用户口味数据库**：喜欢/不喜欢反馈持续累积为标签权重（`userdata.json`），推荐随使用越来越精准。
3. **"试试这个"**：根据你的口味画像，从 Modrinth 最新/热门 mod 中挑出你没评价过的新 mod。
4. **版本过滤与冲突检测**：按游戏版本、加载器自动过滤不兼容 mod；组包时检测冲突风险并汇报。
5. **整合包修复**：把启动器报错（如"缺少 xxx 依赖"）贴给它，agent 自动找到缺失 mod 并补进原整合包，重新拖入启动器即可。
6. **灵活的 LLM 接入**：任何 OpenAI 兼容 API（DeepSeek、OpenAI 等）或本地部署模型（如 Ollama）均可，只需改配置文件。

## 快速开始

### 环境要求

- [Rust](https://rustup.rs/) 1.70+
- 一个 LLM API Key（OpenAI 兼容接口），或本地 Ollama

### 安装与配置

```bash
git clone <本仓库地址>
cd RustAgent
copy config.example.toml config.toml   # Linux/macOS 用 cp
```

编辑 `config.toml`，填入你的 API 信息：

```toml
[llm]
base_url = "https://api.deepseek.com/v1"   # 任意 OpenAI 兼容端点
api_key = "sk-xxxx"                        # 换成你的 key
model = "deepseek-chat"
context_length = 32768                     # 上下文长度
price_input_per_m = 0.27                   # 输入 token 单价 (元/百万)
price_output_per_m = 1.1                   # 输出 token 单价 (元/百万)
token_budget = 2000000                     # token 预算, 用完自动中断
max_tool_iterations = 16                   # 一轮对话最多工具调用次数, 复杂组包需求可调大

[output]
download_dir = "./downloads"               # 整合包输出目录

[database]
path = "./userdata.json"                   # 用户口味数据库
```

### 启动

```bash
cargo run
```

进入交互式终端后直接对话即可，界面采用仿 opencode / claude code 风格：圆角边框横幅、`❯` 提示符（激活预设时显示标签）、工具调用彩色状态行。示例：

```
╭──────────────────────────────────────────╮
│ ⛏ RustAgent v0.2.0 · 测试版              │
│ 模型 deepseek-chat · mod 数据来自 Modrinth │
╰──────────────────────────────────────────╯
❯ 我想要 1.21.1 fabric 的生存整合包, 带点探索和装饰内容
```

agent 会先展示候选 mod 列表（含推荐理由和下载量），你确认后它才下载并生成整合包，输出路径会直接打印在终端。

## 交互命令

| 命令                   | 作用                                                         |
| ---------------------- | ------------------------------------------------------------ |
| `/set`               | 会话预设: `/set 版本=1.21.1 加载器=fabric 数量=10`，之后每条消息自动注入预设，agent 不再追问；数量单次对话上限 20 |
| `/new`               | 开启新会话（清空当前上下文）                                 |
| `/save`              | 保存当前会话到`sessions/`（含对话、token 用量，JSON 格式） |
| `/load`              | 加载最近一次保存的会话                                       |
| `/stats`             | 查看用户口味数据库、标签权重与本次会话用量统计               |
| `/quit` 或 `/exit` | 退出                                                         |
| `Ctrl+C`             | 打断当前正在执行的任务（不会退出程序）                       |

## 子命令

```bash
cargo run -- selftest                 # 不耗 LLM token 的自检: 测试 Modrinth 搜索与组包、校验 .mrpack 格式
cargo run -- demo                     # 无 LLM 的组包逻辑演示 (手动输入版本/加载器/主题)
cargo run -- demo trythis             # 无 LLM 的"试试这个"推荐演示
cargo run -- ui                       # 启动 Web 界面 (默认 http://127.0.0.1:1780, 自动打开浏览器)
cargo run -- repair "{\"pack_name\":\"包名\",\"add_slugs\":[\"sodium\"]}"  # 向已生成的整合包补入指定 mod
```

## Web 界面

运行 `cargo run -- ui` 后，浏览器会自动打开本地 Web 界面（端口在 `config.toml` 的 `[ui]` 段配置，默认 `1780`）：

- **聊天区**：与 CLI 同一套 Agent 对话，支持流式输出——你可以实时看到 Agent 正在调用哪个工具，以及组包时的实时进展（如"正在收集 mod sodium (2/8)"），模型思考时也有"思考中"提示，不再是黑盒等待。
- **预设选项栏**：输入框上方可固定选择 MC 版本、加载器与找包数量（5/8/10/15/20 或自定义，单次对话上限 20，自动记忆），随每条消息一并发给 agent，省去"你想要什么版本"的追问，少花一轮 API 调用。单包用户所选 mod 上限 100 个（前置依赖自动补全与报错修复补入不计入）—— 为考虑轻量化，敬请谅解。
- **整合包管理**：侧栏列出已生成的 `.mrpack`，点"打开目录"按钮直接在文件管理器中打开输出文件夹，拖进启动器即装。
- **会话管理**：侧栏可新会话/保存/加载，会话记录列表点击即导入并完整渲染历史对话（合法文件才可导入）；也可打开 sessions 目录手动放入会话文件导入。
- **任务打断**：Agent 执行期间（模型思考 / 组包收集等长任务）输入区会出现红色"⏸ 打断"按钮，点击即安全中止当前任务——已产生的对话上下文保持合法，随时可以继续下一轮。
- **侧栏面板**：会话管理（新会话/保存/加载）、token 用量统计、用户口味标签权重、可用工具列表、已生成整合包列表。
- 界面以静态三件套（HTML/CSS/JS）内嵌进二进制，单文件即可分发，无需额外部署前端。

## 生成物说明

| 路径                   | 内容                                     |
| ---------------------- | ---------------------------------------- |
| `downloads/*.mrpack` | 整合包文件，可直接拖入 PCL2 等启动器安装 |
| `userdata.json`      | 用户口味数据库（反馈记录、标签权重）     |
| `sessions/*.json`    | 会话存档（完整上下文 + token 用量）      |

## Agent 背后的工具

agent 通过函数调用（tool calling）驱动以下工具，所有 mod 数据均来自 Modrinth 真实 API，不依赖模型记忆：

- `search_mods` — 搜索 mod（支持中文关键词自动翻译、版本/加载器过滤）
- `build_modpack` — 组包：解析依赖闭包、检测冲突、生成 `.mrpack`
- `record_feedback` — 记录喜欢/不喜欢
- `get_user_profile` — 读取口味画像用于个性化推荐
- `recommend_new_mods` — "试试这个"推荐
- `repair_pack` — 向已有整合包补入缺失 mod

## 项目结构

```
src/
├── main.rs       # 入口: 交互式 CLI、子命令分发
├── agent.rs      # Agent 主循环: LLM 对话、工具调度、上下文裁剪、预算控制
├── llm.rs        # OpenAI 兼容 LLM 客户端 (含 tool calling)
├── tools.rs      # 工具注册表: 搜索/组包/反馈/修复的具体实现
├── modrinth.rs   # Modrinth API 客户端
├── pipeline.rs   # 纯逻辑管线: 排序、口味标签、依赖闭包 (可脱离 LLM 演示)
├── database.rs   # 用户口味数据库
├── history.rs    # 会话保存/加载
├── config.rs     # config.toml 解析
└── ui/           # Web 界面: axum 服务器 + REST/流式 API + 内嵌前端三件套
```

## License

见 [LICENSE](LICENSE)。
