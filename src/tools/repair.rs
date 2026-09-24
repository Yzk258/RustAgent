//! repair_pack 工具: 向已生成的整合包补入缺失 mod (加载器从索引 dependencies 推导)。

use super::*;
use std::io::{Read as _, Write};

/// 并发收集单个 mod 的结果 (repair 版): 成功含 file/env, 或记录一条冲突。
/// 与 build.rs 的 CollectOutcome 同构; repair 需在主线程做 existing 查重,
/// 故 file 在结果里原样返回, path 由主线程拼装 (查重发生在 join 之后)。
enum RepairOutcome {
    Mod {
        slug: String,
        version: String,
        file: crate::providers::modrinth::VersionFile,
        client_env: String,
        server_env: String,
    },
    Conflict {
        slug: String,
        issue: String,
    },
}

impl super::ToolRegistry {
    pub(super) async fn repair_pack(&self, args: &str, ctx: &TaskCtx) -> Result<serde_json::Value> {
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
        let pack_path = resolve_pack_path(&self.download_dir, &a.pack_name, &safe)?;

        let pack_file = std::fs::File::open(&pack_path)?;
        let mut archive = zip::ZipArchive::new(pack_file)?;
        let mut index_json = String::new();
        archive
            .by_name("modrinth.index.json")?
            .read_to_string(&mut index_json)?;
        let mut index: serde_json::Value = serde_json::from_str(&index_json)?;
        let deps = index
            .get("dependencies")
            .and_then(|d| d.as_object())
            .ok_or_else(|| anyhow::anyhow!("索引缺少 dependencies 对象, 请重新组包"))?;
        let game_version = deps
            .get("minecraft")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("索引缺少 minecraft 版本声明"))?
            .to_string();
        let loader = if deps.get("fabric-loader").is_some() {
            "fabric"
        } else if deps.get("quilt-loader").is_some() {
            "quilt"
        } else if deps.get("forge").is_some() {
            "forge"
        } else if deps.get("neoforge").is_some() {
            "neoforge"
        } else {
            bail!("索引未声明加载器, 请重新组包");
        };

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
        let (final_slugs, auto_added, closure_conflicts, resolved) =
            crate::pipeline::dependency_closure(
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
        // 并发收集 (跨 slug 并发), 复用 build_modpack 的模式。
        // 优先复用 dependency_closure 已解析的 resolved 缓存 (省 versions + project);
        // repair 的 add_slugs 都是种子, version 全命中, env 需 fallback (种子无缓存 env)。
        let total = final_slugs.len();
        let resolved = std::sync::Arc::new(resolved);
        let mut set = tokio::task::JoinSet::new();
        for slug in final_slugs.iter() {
            let client = self.modrinth.clone();
            let slug = slug.clone();
            let game_version = game_version.clone();
            let loader = loader.to_string();
            let resolved = resolved.clone();
            set.spawn(async move {
                let (v, cached_env) = if let Some(r) = resolved.get(&slug) {
                    (r.version.clone(), r.env.clone())
                } else {
                    let versions = match client.versions(&slug, &game_version, &loader).await {
                        Ok(v) => v,
                        Err(e) => {
                            return RepairOutcome::Conflict {
                                slug,
                                issue: format!("{e:#}"),
                            };
                        }
                    };
                    match crate::providers::modrinth::latest_version(versions) {
                        Some(v) => (v, None),
                        None => {
                            return RepairOutcome::Conflict {
                                slug,
                                issue: format!("无 {game_version}/{loader} 兼容版本"),
                            };
                        }
                    }
                };
                let file = match v
                    .files
                    .iter()
                    .find(|f| f.primary)
                    .or_else(|| v.files.first())
                {
                    Some(f) => f,
                    None => {
                        return RepairOutcome::Conflict {
                            slug,
                            issue: "兼容版本没有可下载文件".to_string(),
                        };
                    }
                };
                let (client_env, server_env) = if let Some(e) = cached_env {
                    e
                } else {
                    match client.project(&slug).await {
                        Ok(p) => (p.client_side, p.server_side),
                        Err(_) => ("required".to_string(), "required".to_string()),
                    }
                };
                RepairOutcome::Mod {
                    slug,
                    version: v.version_number.clone(),
                    file: file.clone(),
                    client_env,
                    server_env,
                }
            });
        }
        let mut collected: Vec<RepairOutcome> = Vec::with_capacity(total);
        loop {
            let joined = tokio::select! {
                j = set.join_next() => j,
                _ = ctx.await_interrupt() => {
                    set.abort_all();
                    bail!(super::TOOL_INTERRUPTED);
                }
            };
            match joined {
                Some(Ok(o)) => collected.push(o),
                Some(Err(_)) => {}
                None => break,
            }
        }
        let mut done = 0u64;
        for outcome in collected {
            done += 1;
            let slug = match &outcome {
                RepairOutcome::Mod { slug, .. } | RepairOutcome::Conflict { slug, .. } => slug,
            };
            ctx.report(
                format!("正在收集 mod {slug} ({}/{})", done, total),
                Some(done),
                Some(total as u64),
            );
            match outcome {
                RepairOutcome::Mod {
                    slug,
                    version,
                    file,
                    client_env,
                    server_env,
                } => {
                    let path = format!("mods/{}", file.filename);
                    if existing.contains(&path) {
                        continue;
                    }
                    existing.insert(path.clone());
                    added.push(json!({ "slug": slug, "version": version, "path": path }));
                    files_arr.push(json!({
                        "path": path,
                        "hashes": { "sha1": file.hashes.sha1, "sha512": file.hashes.sha512 },
                        "env": { "client": client_env, "server": server_env },
                        "downloads": [file.url],
                        "fileSize": file.size,
                    }));
                }
                RepairOutcome::Conflict { slug, issue } => {
                    conflicts.push(format!("{slug}: {issue}"));
                }
            }
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

/// 在 download_dir 找指定名称的 .mrpack: 精确匹配优先, 模糊兜底, 多义报错。
/// 抽成独立函数便于单测三种情况 (精确/模糊单中/多义)。
fn resolve_pack_path(
    dir: &std::path::Path,
    pack_name: &str,
    safe_lower: &str,
) -> Result<std::path::PathBuf> {
    let candidates: Vec<std::path::PathBuf> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "mrpack"))
        .collect();
    let stem_lower = |p: &std::path::Path| {
        p.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("")
            .to_lowercase()
    };
    // 1. 精确匹配 (大小写不敏感)
    if let Some(p) = candidates.iter().find(|p| stem_lower(p) == safe_lower) {
        return Ok(p.clone());
    }
    // 2. 模糊兜底: stem 含包名 (单向)
    let fuzzy: Vec<_> = candidates
        .iter()
        .filter(|p| stem_lower(p).contains(safe_lower))
        .collect();
    match fuzzy.len() {
        0 => bail!("在 {} 中找不到整合包 '{}'", dir.display(), pack_name),
        1 => Ok(fuzzy[0].clone()),
        _ => {
            let names: Vec<String> = fuzzy
                .iter()
                .filter_map(|p| p.file_stem().and_then(|s| s.to_str()).map(String::from))
                .collect();
            bail!(
                "整合包 '{}' 模糊匹配到多个, 请指定完整名称: {}",
                pack_name,
                names.join(", ")
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn touch(dir: &std::path::Path, name: &str) {
        std::fs::File::create(dir.join(name)).unwrap();
    }

    #[test]
    fn resolve_pack_exact_match_preferred() {
        let dir = std::env::temp_dir().join(format!("rustagent-repair-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        touch(&dir, "MyPack.mrpack");
        touch(&dir, "MyPack-old.mrpack");
        // 精确匹配优先于模糊
        let p = resolve_pack_path(&dir, "MyPack", "mypack").unwrap();
        assert_eq!(p.file_name().unwrap(), "MyPack.mrpack");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_pack_fuzzy_single_match_ok() {
        let dir = std::env::temp_dir().join(format!("rustagent-repair-fz-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        touch(&dir, "adventure-pack.mrpack");
        // 无精确, 单一模糊命中
        let p = resolve_pack_path(&dir, "adventure", "adventure").unwrap();
        assert_eq!(p.file_name().unwrap(), "adventure-pack.mrpack");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_pack_ambiguous_fuzzy_errors() {
        let dir = std::env::temp_dir().join(format!("rustagent-repair-amb-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        touch(&dir, "test-pack.mrpack");
        touch(&dir, "test-v2.mrpack");
        // 多个模糊匹配: 应报错而非猜
        let err = resolve_pack_path(&dir, "test", "test").unwrap_err();
        let msg = format!("{err:#}");
        assert!(msg.contains("多个"), "应报多义错误, 实际: {msg}");
        assert!(msg.contains("test-pack"));
        assert!(msg.contains("test-v2"));
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn resolve_pack_not_found_errors() {
        let dir = std::env::temp_dir().join(format!("rustagent-repair-none-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        touch(&dir, "other.mrpack");
        let err = resolve_pack_path(&dir, "missing", "missing").unwrap_err();
        assert!(format!("{err:#}").contains("找不到"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
