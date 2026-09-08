//! 工具注册与执行中心: search_mods / build_modpack / record_feedback /
//! get_user_profile / recommend_new_mods / repair_pack。
//! 本文件只放基础设施 (进展/打断上下文) 与注册分发, 各工具实现在同级子模块。

mod build;
mod feedback;
mod loader_meta;
mod repair;
mod search;

use crate::prelude::*;
use std::collections::{BTreeMap, HashSet};
use std::path::{Path, PathBuf};

use crate::llm::ToolDef;
use crate::providers::modrinth::ModrinthClient;

/// 工具执行过程中的阶段性进展更新: 文本必有, 数值进度可选 (驱动 CLI 进度条 / Web 进度条)。
#[derive(Clone)]
pub struct ProgressUpdate {
    pub text: String,
    pub current: Option<u64>,
    pub total: Option<u64>,
}

pub type ProgressTx = tokio::sync::mpsc::UnboundedSender<ProgressUpdate>;

/// 协作式打断标记 (与 Agent 的 interrupt 同一实例)
pub type InterruptFlag = Arc<AtomicBool>;

/// 工具被打断时抛出的错误标记, agent 据此跳过后续 LLM 调用直接收尾
pub const TOOL_INTERRUPTED: &str = "用户已打断";

/// 长任务上下文: 进展上报 + 打断标记。约定执行超过 3s 的工具必须:
/// 1. 通过 report 上报阶段性进展 (UI/CLI 实时渲染);
/// 2. 在循环等可等待间隙调用 check_interrupt (让打断请求秒级生效)。
pub struct TaskCtx {
    pub progress: Option<ProgressTx>,
    pub interrupt: Option<InterruptFlag>,
}

impl TaskCtx {
    pub fn none() -> Self {
        Self {
            progress: None,
            interrupt: None,
        }
    }

    pub fn report(&self, text: impl Into<String>, current: Option<u64>, total: Option<u64>) {
        if let Some(tx) = &self.progress {
            let _ = tx.send(ProgressUpdate {
                text: text.into(),
                current,
                total,
            });
        }
    }

    pub fn interrupted(&self) -> bool {
        self.interrupt
            .as_ref()
            .is_some_and(|f| f.load(Ordering::Relaxed))
    }

    /// 打断检查点: 已请求打断则立即中止工具 (错误串带 TOOL_INTERRUPTED 标记)
    pub fn check_interrupt(&self) -> Result<()> {
        if self.interrupted() {
            bail!("{TOOL_INTERRUPTED}");
        }
        Ok(())
    }
}

/// 单包用户所选 mod 数上限 (多次对话再合包同样按此判断)。
/// 注意: 依赖自动补全 (auto_added) 与启动器报错触发的修复补入 (repair_pack) 不计入 ——
/// 这两类是正确性需要, 不是用户主动扩包。设上限是为了考虑轻量化, 敬请谅解。
const MAX_USER_MODS_PER_PACK: usize = 100;

pub struct ToolRegistry {
    modrinth: ModrinthClient,
    /// CurseForge 点名客户端 (config.toml [curseforge] enabled 开启时才有)
    cf: Option<crate::providers::curseforge::CfClient>,
    download_dir: PathBuf,
    db_path: String,
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    #[serde(default)]
    game_version: Option<String>,
    #[serde(default)]
    loader: Option<String>,
    #[serde(default)]
    limit: Option<u32>,
}

#[derive(Deserialize)]
struct BuildArgs {
    name: String,
    game_version: String,
    loader: String,
    #[serde(default)]
    mod_slugs: Vec<String>,
    /// CurseForge 独占 mod 的 slug (search_mods 回退查询返回的 source:"curseforge" 候选)
    #[serde(default)]
    cf_mods: Vec<String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct PackIndex {
    format_version: u32,
    game: String,
    version_id: String,
    name: String,
    summary: String,
    files: Vec<IndexFile>,
    dependencies: BTreeMap<String, String>,
}

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct IndexFile {
    path: String,
    hashes: IndexHashes,
    env: Env,
    downloads: Vec<String>,
    file_size: u64,
}

#[derive(Serialize)]
struct IndexHashes {
    sha1: String,
    sha512: String,
}

#[derive(Serialize)]
struct Env {
    client: String,
    server: String,
}

impl ToolRegistry {
    pub fn new(
        modrinth: ModrinthClient,
        cf: Option<crate::providers::curseforge::CfClient>,
        download_dir: impl Into<PathBuf>,
        db_path: impl Into<String>,
    ) -> Self {
        Self {
            modrinth,
            cf,
            download_dir: download_dir.into(),
            db_path: db_path.into(),
        }
    }

    /// CF jar 缓存目录 (用户数据根目录下 cf-cache/, 与 db_path 同级)
    fn cf_cache_dir(&self) -> PathBuf {
        Path::new(&self.db_path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.join("cf-cache"))
            .unwrap_or_else(|| PathBuf::from("cf-cache"))
    }

    /// 设置窗口热切换 CurseForge 支持 (开→装客户端, 关→卸下)
    pub fn set_cf(&mut self, cf: Option<crate::providers::curseforge::CfClient>) {
        self.cf = cf;
    }

    pub fn defs() -> Vec<ToolDef> {
        vec![
            ToolDef::function(
                "search_mods",
                "在 Modrinth 上搜索 Minecraft mod。返回真实存在的 mod 列表及描述, 供用户筛选。Modrinth 无结果且服务器开启 CurseForge 支持时, 会自动用查询词点名尝试 CurseForge (返回的 CF 候选带 source:curseforge 标记)。永远不要凭记忆推荐 mod, 必须调用本工具。",
                json!({
                    "type": "object",
                    "properties": {
                        "query": { "type": "string", "description": "搜索关键词, 如 '装饰' 'explore' '性能优化'" },
                        "game_version": { "type": "string", "description": "Minecraft 版本, 如 1.21.1" },
                        "loader": { "type": "string", "description": "mod 加载器: fabric / forge / neoforge / quilt" },
                        "limit": { "type": "integer", "description": "返回数量, 默认 8" }
                    },
                    "required": ["query"]
                }),
            ),
            ToolDef::function(
                "build_modpack",
                "根据用户确认的 mod 列表生成 .mrpack 整合包文件(可拖入 PCL2 等启动器直接安装)。自动解析每个 mod 的具体版本与前置依赖声明, 并做冲突元检测。支持 fabric / forge / neoforge / quilt 加载器。",
                json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "整合包名称, 将作为输出文件名" },
                        "game_version": { "type": "string", "description": "Minecraft 版本" },
                        "loader": { "type": "string", "description": "mod 加载器: fabric / forge / neoforge / quilt" },
                        "mod_slugs": { "type": "array", "items": { "type": "string" }, "description": "用户确认要加入的 Modrinth mod 的 slug 列表" },
                        "cf_mods": { "type": "array", "items": { "type": "string" }, "description": "CurseForge 独占 mod 的 slug 列表 (search_mods 返回的 source:curseforge 候选放这里, 需服务器已开启 curseforge 支持)" }
                    },
                    "required": ["name", "game_version", "loader"]
                }),
            ),
            ToolDef::function(
                "record_feedback",
                "记录用户对某个 mod 的喜欢/不喜欢反馈, 存入用户数据库, 让未来的推荐更符合口味。只在用户明确表达喜欢或不喜欢后调用。",
                json!({
                    "type": "object",
                    "properties": {
                        "slug": { "type": "string", "description": "mod 的 slug" },
                        "verdict": { "type": "string", "enum": ["like", "dislike"], "description": "用户的态度" }
                    },
                    "required": ["slug", "verdict"]
                }),
            ),
            ToolDef::function(
                "get_user_profile",
                "查看用户口味画像: 历史反馈统计与标签偏好权重。在挑选和排序候选前调用, 以便个性化推荐。",
                json!({ "type": "object", "properties": {} }),
            ),
            ToolDef::function(
                "recommend_new_mods",
                "\"试试这个\": 根据用户口味从 Modrinth 最新/热门 mod 中推荐用户未评价过的新 mod。",
                json!({
                    "type": "object",
                    "properties": {
                        "game_version": { "type": "string", "description": "Minecraft 版本" },
                        "loader": { "type": "string", "description": "mod 加载器" },
                        "count": { "type": "integer", "description": "推荐数量, 默认 5" }
                    },
                    "required": ["game_version", "loader"]
                }),
            ),
            ToolDef::function(
                "repair_pack",
                "向已生成的整合包补充缺失的 mod。当用户贴出启动器报错(如'缺少 xxx 依赖')时: 先用 search_mods 找到缺失 mod 的 slug, 再调用本工具把它补进原包。",
                json!({
                    "type": "object",
                    "properties": {
                        "pack_name": { "type": "string", "description": "要修复的整合包名称(生成时的 name)" },
                        "add_slugs": { "type": "array", "items": { "type": "string" }, "description": "要补入的 mod slug 列表" }
                    },
                    "required": ["pack_name", "add_slugs"]
                }),
            ),
        ]
    }

    pub async fn execute(
        &self,
        name: &str,
        arguments: &str,
        ctx: &TaskCtx,
    ) -> Result<serde_json::Value> {
        match name {
            "search_mods" => self.search_mods(arguments).await,
            "build_modpack" => self.build_modpack(arguments, ctx).await,
            "record_feedback" => self.record_feedback(arguments).await,
            "get_user_profile" => self.get_user_profile().await,
            "recommend_new_mods" => self.recommend_new(arguments).await,
            "repair_pack" => self.repair_pack(arguments, ctx).await,
            _ => bail!("未知工具: {name}"),
        }
    }
}
