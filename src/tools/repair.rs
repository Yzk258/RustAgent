//! repair_pack 工具: 向已生成的整合包补入缺失 mod (加载器从索引 dependencies 推导)。

use super::*;
use std::io::{Read as _, Write};

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
