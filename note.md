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

- Git 最新提交：`732af1e demo测试版`（v0.2 测试版）。
- CLI REPL 已有：/new /save /load /stats /quit，Ctrl+C 打断任务不退出。
- 已实现：Modrinth 检索与下载、兼容性检查、repair_pack、demo 流水线、token 统计、历史管理。

## 待办 / TODO

- [ ] 优化根据prompt随机找包机制（固定公式->带有一定随机分布避免重复）
- [ ] "试试这个" 定期推荐窗口
- [ ] Web 界面（视 CLI 稳定后扩展至pcl2启动器）
- [ ] token 显示与预算上限自动中断机制
- [ ] 冲突检测基于元数据的更细粒度报告
- [ ] 用户口味数据库的反馈闭环完善
- [ ] 组完包后自动化拖入pcl2启动器纠错的可选机制补充
- [ ] 组包mod数量可选择化（1-10、10-20...）、mod条件可选择化（从最新或是最热两种模式找mod）
