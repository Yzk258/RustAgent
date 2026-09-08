//! build_modpack 工具: 依赖闭包 + CurseForge 独占收集 + 生成 .mrpack。

use super::*;
use std::io::Write;

impl super::ToolRegistry {
    pub(super) async fn build_modpack(
        &self,
        args: &str,
        ctx: &TaskCtx,
    ) -> Result<serde_json::Value> {
        let a: BuildArgs = serde_json::from_str(args)?;
        if a.mod_slugs.is_empty() && a.cf_mods.is_empty() {
            bail!("mod 列表为空");
        }
        // 只统计用户主动挑选的 mod (Modrinth + CurseForge); 依赖补全走 auto_added,
        // 修复补入走 repair_pack, 均不计入 —— 设上限是为考虑轻量化, 敬请谅解。
        let user_count = a.mod_slugs.len() + a.cf_mods.len();
        if user_count > MAX_USER_MODS_PER_PACK {
            bail!(
                "所选 mod {user_count} 个, 超出单包上限 {MAX_USER_MODS_PER_PACK} (依赖自动补全与报错修复补入不计入) —— 为考虑轻量化, 敬请谅解"
            );
        }
        let dep_key = super::loader_meta::loader_dep_key(&a.loader)?;

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
        let (mut final_slugs, mut auto_added, closure_conflicts) =
            crate::pipeline::dependency_closure(
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

        // CurseForge 独占 mod 收集: cfwidget 点名 → 选版 → 直链下载算双哈希 → jar 缓存。
        // 前置声明从 jar 内 manifest 读取 (fabric.mod.json / mods.toml)。
        let mut cf_dep_ids: Vec<String> = Vec::new();
        if !a.cf_mods.is_empty() {
            let Some(cf) = &self.cf else {
                bail!("cf_mods 需要在 config.toml 的 [curseforge] 打开 enabled 后使用");
            };
            for (i, slug) in a.cf_mods.iter().enumerate() {
                ctx.check_interrupt()?;
                ctx.report(
                    format!("解析 CF mod {slug} ({}/{})", i + 1, a.cf_mods.len()),
                    Some((i + 1) as u64),
                    Some(a.cf_mods.len() as u64),
                );
                let proj = match cf.lookup(slug).await {
                    Ok(p) => p,
                    Err(e) => {
                        conflicts.push(
                            json!({ "slug": slug, "issue": format!("cfwidget 查询失败: {e:#}") }),
                        );
                        continue;
                    }
                };
                let file = match crate::providers::curseforge::select_file(
                    &proj.files,
                    &a.game_version,
                    &a.loader,
                ) {
                    Some(f) => f,
                    None => {
                        conflicts.push(json!({ "slug": slug, "issue": format!("CF 上没有 {} / {} 的兼容文件", a.game_version, a.loader) }));
                        continue;
                    }
                };
                let meta = match cf.fetch_file(&proj, file, &self.cf_cache_dir(), ctx).await {
                    Ok(m) => m,
                    Err(e) => {
                        conflicts.push(
                            json!({ "slug": slug, "issue": format!("CF 文件获取失败: {e:#}") }),
                        );
                        continue;
                    }
                };
                let (client_env, server_env) = crate::providers::curseforge::env_from_file(file);
                total_size += meta.size;
                summaries.push(json!({
                    "slug": slug,
                    "source": "curseforge",
                    "filename": meta.filename,
                    "size_mb": format!("{:.2}", meta.size as f64 / 1_048_576.0),
                }));
                files.push(IndexFile {
                    path: format!("mods/{}", meta.filename),
                    hashes: IndexHashes {
                        sha1: meta.sha1.clone(),
                        sha512: meta.sha512.clone(),
                    },
                    env: Env {
                        client: client_env,
                        server: server_env,
                    },
                    downloads: vec![meta.url.clone()],
                    file_size: meta.size,
                });
                seen.insert(format!("cf:{slug}"));
                cf_dep_ids.extend(meta.depends);
            }
            // CF manifest 声明的前置: Modrinth 有同名项目则并入收集 (fabric-api 等常见前置都在 Modrinth)
            let mut extra: Vec<String> = Vec::new();
            for dep in cf_dep_ids {
                let key = format!("cf:{dep}");
                if seen.contains(&key) || final_slugs.contains(&dep) || extra.contains(&dep) {
                    continue;
                }
                seen.insert(key);
                if self.modrinth.project(&dep).await.is_ok() {
                    extra.push(dep);
                } else {
                    conflicts.push(json!({ "slug": dep, "issue": "CF mod 声明的前置在 Modrinth 无同名项目, 请提醒用户手动确认" }));
                }
            }
            auto_added.extend(extra.iter().cloned());
            final_slugs.extend(extra);
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
            bail!(
                "没有任何 mod 能解析出兼容版本, 组包中止. 详情: {}",
                serde_json::to_string(&conflicts).unwrap_or_default()
            );
        }

        ctx.report(format!("获取 {} loader 版本", a.loader), None, None);
        let loader_version = self.loader_version(&a.loader, &a.game_version).await?;
        let mut deps = BTreeMap::new();
        deps.insert("minecraft".to_string(), a.game_version.clone());
        deps.insert(dep_key.to_string(), loader_version);

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

        // 记录组包到用户数据库 (失败不阻断已生成的 .mrpack); 记录含 CF 条目 (cf: 前缀区分来源)
        let mut record_slugs = final_slugs.clone();
        record_slugs.extend(a.cf_mods.iter().map(|s| format!("cf:{s}")));
        let mut db = crate::storage::database::UserDatabase::load(&self.db_path);
        db.packs.push(crate::storage::database::PackRecord {
            name: a.name.clone(),
            mod_slugs: record_slugs,
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read as _;

    /// 端到端真实网络测试: CF 点名 (暮色森林) → 直链下载算哈希 → manifest 前置
    /// → fabric-api 从 Modrinth 补全 → 混合来源 .mrpack 生成。手动运行:
    /// `cargo test --lib cf_build_modpack_live -- --ignored --nocapture`
    #[tokio::test]
    #[ignore = "真实网络: cfwidget + CF 直链下载 ~30MB"]
    async fn cf_build_modpack_live() {
        let dir = std::env::temp_dir().join(format!("rustagent-cfpack-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let registry = ToolRegistry::new(
            crate::providers::modrinth::ModrinthClient::new().unwrap(),
            Some(crate::providers::curseforge::CfClient::new()),
            dir.join("packs"),
            dir.join("t.db").to_string_lossy().to_string(),
        );
        let args = r#"{"name":"cf-live","game_version":"1.21.1","loader":"fabric","mod_slugs":[],"cf_mods":["the-twilight-forest"]}"#;
        let out = registry
            .execute("build_modpack", args, &TaskCtx::none())
            .await
            .unwrap();
        println!("{}", serde_json::to_string_pretty(&out).unwrap());
        assert!(
            out["mod_count"].as_u64().unwrap() >= 2,
            "应含暮色森林 + 自动补全的 fabric-api, 实际: {out}"
        );
        let mut z = zip::ZipArchive::new(
            std::fs::File::open(out["output_path"].as_str().unwrap()).unwrap(),
        )
        .unwrap();
        let mut idx = String::new();
        z.by_name("modrinth.index.json")
            .unwrap()
            .read_to_string(&mut idx)
            .unwrap();
        let v: serde_json::Value = serde_json::from_str(&idx).unwrap();
        let files = v["files"].as_array().unwrap();
        assert_eq!(files.len(), 2, "混合包应恰好两个文件: {v}");
        assert!(files
            .iter()
            .any(|f| f["downloads"][0].as_str().unwrap().contains("forgecdn")));
        assert!(files
            .iter()
            .any(|f| f["downloads"][0].as_str().unwrap().contains("modrinth")));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
