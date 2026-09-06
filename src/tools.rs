use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::collections::{BTreeMap, HashSet};
use std::io::{Read as _, Write};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use crate::llm::ToolDef;
use crate::modrinth::ModrinthClient;

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
    mod_slugs: Vec<String>,
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
        download_dir: impl Into<PathBuf>,
        db_path: impl Into<String>,
    ) -> Self {
        Self {
            modrinth,
            download_dir: download_dir.into(),
            db_path: db_path.into(),
        }
    }

    pub fn defs() -> Vec<ToolDef> {
        vec![
            ToolDef::function(
                "search_mods",
                "在 Modrinth 上搜索 Minecraft mod。返回真实存在的 mod 列表及描述, 供用户筛选。永远不要凭记忆推荐 mod, 必须调用本工具。",
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
                "根据用户确认的 mod 列表生成 .mrpack 整合包文件(可拖入 PCL2 等启动器直接安装)。自动解析每个 mod 的具体版本与前置依赖声明, 并做冲突元检测。",
                json!({
                    "type": "object",
                    "properties": {
                        "name": { "type": "string", "description": "整合包名称, 将作为输出文件名" },
                        "game_version": { "type": "string", "description": "Minecraft 版本" },
                        "loader": { "type": "string", "description": "mod 加载器" },
                        "mod_slugs": { "type": "array", "items": { "type": "string" }, "description": "用户确认要加入的 mod 的 slug 列表" }
                    },
                    "required": ["name", "game_version", "loader", "mod_slugs"]
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

    async fn fabric_loader_version(&self) -> Result<String> {
        #[derive(Deserialize)]
        struct Entry {
            version: String,
            stable: bool,
        }
        let entries: Vec<Entry> = self
            .modrinth
            .http()
            .get("https://meta.fabricmc.net/v2/versions/loader")
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        entries
            .into_iter()
            .find(|e| e.stable)
            .map(|e| e.version)
            .ok_or_else(|| anyhow::anyhow!("fabric meta 无稳定版本"))
    }

    async fn search_mods(&self, args: &str) -> Result<serde_json::Value> {
        let a: SearchArgs = serde_json::from_str(args)?;
        let query = crate::pipeline::translate_keyword(&a.query);
        let mut facets: Vec<Vec<String>> = Vec::new();
        if let Some(gv) = &a.game_version {
            facets.push(vec![format!("versions:{gv}")]);
        }
        if let Some(l) = &a.loader {
            facets.push(vec![format!("categories:{l}")]);
        }
        let resp = self
            .modrinth
            .search(
                &query,
                if facets.is_empty() {
                    None
                } else {
                    Some(facets)
                },
                a.limit.unwrap_or(8).clamp(1, 20),
                "relevance",
            )
            .await?;
        let mods: Vec<serde_json::Value> = resp
            .hits
            .iter()
            .map(|h| {
                let recent: Vec<String> = h.versions.iter().rev().take(4).cloned().collect();
                json!({
                    "slug": h.slug,
                    "title": h.title,
                    "author": h.author,
                    "downloads": h.downloads,
                    "description": h.description,
                    "categories": h.display_categories,
                    "recent_game_versions": recent,
                })
            })
            .collect();
        Ok(json!({ "query_used": query, "total_hits": resp.total_hits, "mods": mods }))
    }

    async fn build_modpack(&self, args: &str, ctx: &TaskCtx) -> Result<serde_json::Value> {
        let a: BuildArgs = serde_json::from_str(args)?;
        if a.mod_slugs.is_empty() {
            bail!("mod 列表为空");
        }
        // 只统计用户主动挑选的 mod; 依赖补全走 auto_added, 修复补入走 repair_pack, 均不计入
        if a.mod_slugs.len() > MAX_USER_MODS_PER_PACK {
            bail!(
                "所选 mod {} 个, 超出单包上限 {MAX_USER_MODS_PER_PACK} (依赖自动补全与报错修复补入不计入) —— 为考虑轻量化, 敬请谅解",
                a.mod_slugs.len()
            );
        }
        if a.loader != "fabric" {
            bail!("v0 暂只支持 fabric 加载器");
        }

        let mut seen: HashSet<String> = HashSet::new();
        let mut files: Vec<IndexFile> = Vec::new();
        let mut summaries: Vec<serde_json::Value> = Vec::new();
        let mut conflicts: Vec<serde_json::Value> = Vec::new();
        let mut total_size: u64 = 0;

        ctx.report(
            format!("解析依赖闭包 ({} 个 mod)", a.mod_slugs.len()),
            None,
            None,
        );
        let (final_slugs, auto_added, closure_conflicts) = crate::pipeline::dependency_closure(
            &self.modrinth,
            &a.game_version,
            &a.loader,
            &a.mod_slugs,
            ctx,
        )
        .await?;
        ctx.report(
            format!(
                "依赖闭包解析完成: 共 {} 个 mod (自动补充前置 {} 个)",
                final_slugs.len(),
                auto_added.len()
            ),
            None,
            None,
        );
        for c in &closure_conflicts {
            conflicts.push(json!({ "issue": c }));
        }

        let total = final_slugs.len();
        for (i, slug) in final_slugs.iter().enumerate() {
            ctx.check_interrupt()?;
            ctx.report(
                format!("正在收集 mod {slug} ({}/{})", i + 1, total),
                Some((i + 1) as u64),
                Some(total as u64),
            );
            if !seen.insert(slug.clone()) {
                conflicts.push(json!({ "slug": slug, "issue": "重复添加" }));
                continue;
            }
            let versions = match self
                .modrinth
                .versions(slug, &a.game_version, &a.loader)
                .await
            {
                Ok(v) => v,
                Err(e) => {
                    conflicts
                        .push(json!({ "slug": slug, "issue": format!("无法获取版本信息: {e:#}") }));
                    continue;
                }
            };
            let v = match versions.first() {
                Some(v) => v,
                None => {
                    conflicts.push(json!({ "slug": slug, "issue": format!("没有 {} / {} 的兼容版本", a.game_version, a.loader) }));
                    continue;
                }
            };
            let Some(file) = v
                .files
                .iter()
                .find(|f| f.primary)
                .or_else(|| v.files.first())
            else {
                conflicts.push(json!({ "slug": slug, "issue": "兼容版本没有可下载文件" }));
                continue;
            };
            let (client_env, server_env) = match self.modrinth.project(slug).await {
                Ok(p) => (p.client_side, p.server_side),
                Err(_) => ("required".to_string(), "required".to_string()),
            };
            total_size += file.size;
            summaries.push(json!({
                "slug": slug,
                "version": v.version_number,
                "filename": file.filename,
                "size_mb": format!("{:.2}", file.size as f64 / 1_048_576.0),
            }));
            files.push(IndexFile {
                path: format!("mods/{}", file.filename),
                hashes: IndexHashes {
                    sha1: file.hashes.sha1.clone(),
                    sha512: file.hashes.sha512.clone(),
                },
                env: Env {
                    client: client_env,
                    server: server_env,
                },
                downloads: vec![file.url.clone()],
                file_size: file.size,
            });
        }

        if files.is_empty() {
            bail!("没有任何 mod 能解析出兼容版本, 组包中止");
        }

        ctx.report("获取 fabric loader 版本".to_string(), None, None);
        let loader_version = self.fabric_loader_version().await?;
        let mut deps = BTreeMap::new();
        deps.insert("minecraft".to_string(), a.game_version.clone());
        deps.insert("fabric-loader".to_string(), loader_version);

        let index = PackIndex {
            format_version: 1,
            game: "minecraft".into(),
            version_id: format!("rustagent-{}", a.name),
            name: a.name.clone(),
            summary: format!(
                "由 RustAgent 生成的整合包 (MC {} / {})",
                a.game_version, a.loader
            ),
            files,
            dependencies: deps,
        };

        ctx.report(
            format!("写入整合包文件 {} 个 mod", summaries.len()),
            None,
            None,
        );
        std::fs::create_dir_all(&self.download_dir)?;
        let safe_name = a.name.replace(['/', '\\', ':', '*'], "-");
        let out_path = self.download_dir.join(format!("{safe_name}.mrpack"));
        let out_file = std::fs::File::create(&out_path)?;
        let mut zip = zip::ZipWriter::new(out_file);
        zip.start_file(
            "modrinth.index.json",
            zip::write::SimpleFileOptions::default(),
        )?;
        zip.write_all(serde_json::to_string_pretty(&index)?.as_bytes())?;
        zip.finish()?;

        // 记录组包到用户数据库 (失败不阻断已生成的 .mrpack; final_slugs 含用户所选+自动补全的完整闭包)
        let mut db = crate::database::UserDatabase::load(&self.db_path);
        db.packs.push(crate::database::PackRecord {
            name: a.name.clone(),
            mod_slugs: final_slugs.clone(),
            created_at: chrono::Local::now().to_rfc3339(),
        });
        let db_summary = match db.save() {
            Ok(()) => db.summary(),
            Err(_) => String::new(),
        };

        Ok(json!({
            "output_path": out_path.display().to_string(),
            "mod_count": summaries.len(),
            "total_size_mb": format!("{:.1}", total_size as f64 / 1_048_576.0),
            "auto_added": auto_added,
            "mods": summaries,
            "conflicts": conflicts,
            "db_summary": db_summary,
            "note": if conflicts.is_empty() { "无冲突" } else { "存在冲突条目, 请向用户说明" },
        }))
    }

    async fn record_feedback(&self, args: &str) -> Result<serde_json::Value> {
        #[derive(Deserialize)]
        struct Args {
            slug: String,
            verdict: String,
        }
        let a: Args = serde_json::from_str(args)?;
        if a.verdict != "like" && a.verdict != "dislike" {
            bail!("verdict 必须是 like 或 dislike");
        }
        let mut db = crate::database::UserDatabase::load(&self.db_path);
        let tags = match self.modrinth.project(&a.slug).await {
            Ok(p) => crate::pipeline::taste_tags(&p.categories),
            Err(_) => Vec::new(),
        };
        db.rate(crate::database::FeedbackRecord {
            slug: a.slug.clone(),
            verdict: a.verdict.clone(),
            tags: tags.clone(),
            game_version: String::new(),
            loader: String::new(),
            source: "chat".to_string(),
            timestamp: chrono::Local::now().to_rfc3339(),
        });
        db.save()?;
        Ok(json!({
            "status": "已记录",
            "slug": a.slug,
            "verdict": a.verdict,
            "tags": tags,
            "db_summary": db.summary(),
        }))
    }

    async fn get_user_profile(&self) -> Result<serde_json::Value> {
        let db = crate::database::UserDatabase::load(&self.db_path);
        Ok(json!({
            "summary": db.summary(),
            "taste_weights": db.tag_weights(),
            "top_tags": db.top_tags(5),
            "recent_packs": db.packs.iter().rev().take(3).collect::<Vec<_>>(),
        }))
    }

    async fn recommend_new(&self, args: &str) -> Result<serde_json::Value> {
        #[derive(Deserialize)]
        struct Args {
            game_version: String,
            loader: String,
            #[serde(default)]
            count: Option<u32>,
        }
        let a: Args = serde_json::from_str(args)?;
        let db = crate::database::UserDatabase::load(&self.db_path);
        let ranked =
            crate::pipeline::try_this(&self.modrinth, &db, &a.game_version, &a.loader).await?;
        let take = a.count.unwrap_or(5).clamp(1, 10) as usize;
        let mods: Vec<serde_json::Value> = ranked
            .into_iter()
            .take(take)
            .map(|s| {
                json!({
                    "slug": s.hit.slug,
                    "title": s.hit.title,
                    "description": s.hit.description,
                    "downloads": s.hit.downloads,
                    "categories": s.hit.display_categories,
                    "taste_score": s.score,
                })
            })
            .collect();
        Ok(json!({ "recommendations": mods, "db_summary": db.summary() }))
    }

    async fn repair_pack(&self, args: &str, ctx: &TaskCtx) -> Result<serde_json::Value> {
        #[derive(Deserialize)]
        struct Args {
            pack_name: String,
            add_slugs: Vec<String>,
        }
        let a: Args = serde_json::from_str(args)?;
        if a.add_slugs.is_empty() {
            bail!("add_slugs 为空");
        }
        let safe = a.pack_name.to_lowercase();
        let pack_path = std::fs::read_dir(&self.download_dir)?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "mrpack"))
            .find(|p| {
                let stem = p
                    .file_stem()
                    .and_then(|s| s.to_str())
                    .unwrap_or("")
                    .to_lowercase();
                stem.contains(&safe) || safe.contains(&stem)
            })
            .ok_or_else(|| {
                anyhow::anyhow!(
                    "在 {} 中找不到整合包 '{}'",
                    self.download_dir.display(),
                    a.pack_name
                )
            })?;

        let pack_file = std::fs::File::open(&pack_path)?;
        let mut archive = zip::ZipArchive::new(pack_file)?;
        let mut index_json = String::new();
        archive
            .by_name("modrinth.index.json")?
            .read_to_string(&mut index_json)?;
        let mut index: serde_json::Value = serde_json::from_str(&index_json)?;
        let deps = index
            .get("dependencies")
            .ok_or_else(|| anyhow::anyhow!("索引缺少 dependencies"))?;
        if !deps.is_object() {
            bail!("索引为旧格式 (dependencies 非对象), 请重新组包");
        }
        let game_version = deps
            .get("minecraft")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("索引缺少 minecraft 版本声明"))?
            .to_string();
        let loader = "fabric";

        let mut existing: HashSet<String> = index
            .get("files")
            .and_then(|f| f.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|e| e.get("path").and_then(|p| p.as_str()).map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        ctx.report(
            format!("解析依赖闭包 ({} 个 mod)", a.add_slugs.len()),
            None,
            None,
        );
        let (final_slugs, auto_added, closure_conflicts) = crate::pipeline::dependency_closure(
            &self.modrinth,
            &game_version,
            loader,
            &a.add_slugs,
            ctx,
        )
        .await?;
        let mut conflicts: Vec<String> = closure_conflicts;

        let mut added: Vec<serde_json::Value> = Vec::new();
        let files_arr = index
            .get_mut("files")
            .and_then(|f| f.as_array_mut())
            .ok_or_else(|| anyhow::anyhow!("索引 files 异常"))?;
        let total = final_slugs.len();
        for (i, slug) in final_slugs.iter().enumerate() {
            ctx.check_interrupt()?;
            ctx.report(
                format!("正在收集 mod {slug} ({}/{})", i + 1, total),
                Some((i + 1) as u64),
                Some(total as u64),
            );
            let versions = match self.modrinth.versions(slug, &game_version, loader).await {
                Ok(v) => v,
                Err(e) => {
                    conflicts.push(format!("{slug}: {e:#}"));
                    continue;
                }
            };
            let v = match versions.first() {
                Some(v) => v,
                None => {
                    conflicts.push(format!("{slug}: 无 {game_version}/{loader} 兼容版本"));
                    continue;
                }
            };
            let Some(file) = v
                .files
                .iter()
                .find(|f| f.primary)
                .or_else(|| v.files.first())
            else {
                conflicts.push(format!("{slug}: 兼容版本没有可下载文件"));
                continue;
            };
            let path = format!("mods/{}", file.filename);
            if existing.contains(&path) {
                continue;
            }
            let (client_env, server_env) = match self.modrinth.project(slug).await {
                Ok(p) => (p.client_side, p.server_side),
                Err(_) => ("required".to_string(), "required".to_string()),
            };
            existing.insert(path.clone());
            added.push(json!({ "slug": slug, "version": v.version_number, "path": path }));
            files_arr.push(json!({
                "path": path,
                "hashes": { "sha1": file.hashes.sha1, "sha512": file.hashes.sha512 },
                "env": { "client": client_env, "server": server_env },
                "downloads": [file.url],
                "fileSize": file.size,
            }));
        }

        let total_mods = files_arr.len();

        ctx.report(
            format!("写入整合包文件 (共 {total_mods} 个 mod)"),
            Some(total_mods as u64),
            Some(total_mods as u64),
        );
        std::fs::create_dir_all(&self.download_dir)?;
        let out_file = std::fs::File::create(&pack_path)?;
        let mut zip = zip::ZipWriter::new(out_file);
        zip.start_file(
            "modrinth.index.json",
            zip::write::SimpleFileOptions::default(),
        )?;
        zip.write_all(serde_json::to_string_pretty(&index)?.as_bytes())?;
        zip.finish()?;

        Ok(json!({
            "pack": pack_path.display().to_string(),
            "added": added,
            "auto_added_by_closure": auto_added,
            "conflicts": conflicts,
            "total_mods": total_mods,
            "note": if added.is_empty() { "无新增, 所需 mod 已在包中" } else { "已补入, 请用户重新拖入启动器安装" },
        }))
    }
}
