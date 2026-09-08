//! CurseForge 点名支持 (阶段一实验性, 无 API key):
//! - 元数据: cfwidget 公共镜像按 slug 点名查询 (无搜索能力)
//! - 文件: 由 file id 推导 forgecdn CDN 直链直接下载 (www 重定向端点被
//!   Cloudflare JS 挑战拦截, reqwest 无法通过, 已绕开; CDN 无 CF 保护)
//! - 哈希: CF 无 sha512, 下载 jar 后本地计算 sha1+sha512, 结果缓存复用
//! - 依赖: 读 jar 内 fabric.mod.json / META-INF/mods.toml 的 depends 声明
//!
//! 接口非官方契约, CF 收紧风控时可能失效, 调用方需向用户说明属实验性能力。

use crate::prelude::*;
use sha1::{Digest, Sha1};
use sha2::Sha512;
use std::io::{Read as _, Write as _};
use std::path::Path;

const BROWSER_UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/128.0.0.0 Safari/537.36";

/// cfwidget 项目元数据 (只取需要的字段)
#[derive(Deserialize)]
pub struct CfProject {
    pub id: u64,
    pub title: String,
    #[serde(default)]
    pub summary: String,
    #[serde(default)]
    pub categories: Vec<String>,
    #[serde(default)]
    pub files: Vec<CfFile>,
}

#[derive(Deserialize, Clone)]
pub struct CfFile {
    pub id: u64,
    pub name: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub filesize: u64,
    /// 标签数组: MC 版本 ("1.21.1") / 加载器 ("Fabric") / 端 ("Client"/"Server")
    #[serde(default)]
    pub versions: Vec<String>,
}

/// CF 文件落缓存后的完整信息 (sidecar json, 重建包/修复时离线复用)
#[derive(Serialize, Deserialize, Clone)]
pub struct CfFileMeta {
    pub mod_id: u64,
    pub file_id: u64,
    pub filename: String,
    pub url: String,
    pub sha1: String,
    pub sha512: String,
    pub size: u64,
    #[serde(default)]
    pub depends: Vec<String>,
}

fn loader_tag(loader: &str) -> &'static str {
    match loader {
        "fabric" => "Fabric",
        "quilt" => "Quilt",
        "neoforge" => "NeoForge",
        _ => "Forge",
    }
}

/// 从候选文件中选版: 过滤 MC 版本与加载器标签, release 优先, 同类取最新 (file id 单调递增)
pub fn select_file<'a>(
    files: &'a [CfFile],
    game_version: &str,
    loader: &str,
) -> Option<&'a CfFile> {
    let tag = loader_tag(loader);
    files
        .iter()
        .filter(|f| {
            f.versions.iter().any(|v| v == game_version) && f.versions.iter().any(|v| v == tag)
        })
        .max_by_key(|f| (f.kind == "release", f.id))
}

/// 由文件标签推导 .mrpack env 字段 (CF 无 Modrinth 式 client_side 元数据)
pub fn env_from_file(f: &CfFile) -> (String, String) {
    let c = f.versions.iter().any(|t| t == "Client");
    let s = f.versions.iter().any(|t| t == "Server");
    match (c, s) {
        (true, false) => ("required".into(), "unsupported".into()),
        (false, true) => ("unsupported".into(), "required".into()),
        _ => ("required".into(), "required".into()),
    }
}

/// 查询词转 CF slug 形态: 小写 + 空格转短横线 + 去非 ASCII 字符 (中文查询会得到空串)
pub fn slugify(query: &str) -> String {
    let joined: String = query
        .trim()
        .to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join("-");
    joined
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect()
}

/// 读 jar 内前置声明: fabric.mod.json 的 depends 键 / mods.toml 的 dependencies 段。
/// 过滤掉 minecraft/java/loader 本体声明, 只留可能需要补入的 mod id。
pub fn read_depends(jar: &Path) -> Result<Vec<String>> {
    let file = std::fs::File::open(jar)?;
    let mut zip = zip::ZipArchive::new(file)?;
    if let Ok(mut e) = zip.by_name("fabric.mod.json") {
        let mut s = String::new();
        e.read_to_string(&mut s)?;
        let v: serde_json::Value = serde_json::from_str(&s)?;
        if let Some(deps) = v.pointer("/depends").and_then(|d| d.as_object()) {
            const SKIP: [&str; 6] = [
                "minecraft",
                "java",
                "fabricloader",
                "fabric",
                "quilt_loader",
                "quilt_base",
            ];
            return Ok(deps
                .keys()
                .filter(|k| !SKIP.contains(&k.as_str()))
                .cloned()
                .collect());
        }
    }
    if let Ok(mut e) = zip.by_name("META-INF/mods.toml") {
        let mut s = String::new();
        e.read_to_string(&mut s)?;
        if let Ok(t) = s.parse::<toml::Table>() {
            // mods.toml 的 [[dependencies.声明者]] 是数组, 真正的依赖在各条目的 modId 字段
            if let Some(deps) = t.get("dependencies").and_then(|d| d.as_table()) {
                const SKIP: [&str; 4] = ["minecraft", "java", "forge", "neoforge"];
                let mut ids: Vec<String> = Vec::new();
                for arr in deps.values() {
                    if let Some(list) = arr.as_array() {
                        for entry in list {
                            if let Some(id) = entry.get("modId").and_then(|v| v.as_str()) {
                                ids.push(id.to_string());
                            }
                        }
                    }
                }
                ids.sort();
                ids.dedup();
                return Ok(ids
                    .into_iter()
                    .filter(|k| !SKIP.contains(&k.as_str()))
                    .collect());
            }
        }
    }
    Ok(Vec::new())
}

pub struct CfClient {
    http: reqwest::Client,
}

/// 由 file id 推导 forgecdn 直链: 路径 = 前段(id 去掉末 3 位) / 末 3 位(去前导零) / 文件名。
/// www 重定向端点被 Cloudflare JS 挑战拦截 (reqwest 无法通过), 故直接推导绕开;
/// 规则在 4 个真实文件上验证过 (8749067→8749/67, 8792636→8792/636 等), CDN 无 CF 保护。
pub fn cdn_url(file_id: u64, filename: &str) -> String {
    let s = file_id.to_string();
    let (a, b) = s.split_at(s.len().saturating_sub(3));
    let b = b.trim_start_matches('0');
    let b = if b.is_empty() { "0" } else { b };
    format!("https://mediafilez.forgecdn.net/files/{a}/{b}/{filename}")
}

impl Default for CfClient {
    fn default() -> Self {
        Self::new()
    }
}

impl CfClient {
    pub fn new() -> Self {
        let http = reqwest::Client::builder()
            .user_agent(BROWSER_UA)
            .timeout(std::time::Duration::from_secs(300))
            .build()
            .expect("构建 HTTP 客户端失败");
        Self { http }
    }

    /// cfwidget 点名查询 (slug 形如 the-twilight-forest); 未缓存项目会返回排队中, 自动重试
    pub async fn lookup(&self, slug: &str) -> Result<CfProject> {
        let url = format!("https://api.cfwidget.com/minecraft/mc-mods/{slug}");
        let mut last = String::new();
        for attempt in 0..3 {
            if attempt > 0 {
                tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
            }
            let resp = self.http.get(&url).send().await?;
            let status = resp.status();
            let body = resp.text().await?;
            if status.is_success() {
                return serde_json::from_str(&body).context("解析 cfwidget 响应失败");
            }
            last = format!("{status}: {}", &body[..body.len().min(120)]);
            // 202/429 = 排队/限流可重试, 其余 (404 等) 直接放弃
            if status.as_u16() != 202 && status.as_u16() != 429 {
                break;
            }
        }
        bail!("cfwidget 查询 {slug} 失败 ({last})")
    }

    /// 取一个 CF 文件: 命中缓存直接回 sidecar; 否则按 file id 推导 CDN 直链下载
    /// → 边下边算 sha1/sha512 → 读 jar 前置声明 → jar 与 sidecar 落盘缓存。
    /// 下载循环逐块响应打断。
    pub async fn fetch_file(
        &self,
        project: &CfProject,
        file: &CfFile,
        cache_dir: &Path,
        ctx: &crate::tools::TaskCtx,
    ) -> Result<CfFileMeta> {
        let sidecar = cache_dir.join(format!("{}-{}.json", project.id, file.id));
        if let Some(meta) = std::fs::read_to_string(&sidecar)
            .ok()
            .and_then(|s| serde_json::from_str::<CfFileMeta>(&s).ok())
        {
            return Ok(meta);
        }
        let cdn = cdn_url(file.id, &file.name);
        ctx.report(
            format!("正在下载 {} (CurseForge 直链)", file.name),
            None,
            None,
        );
        let resp = self.http.get(&cdn).send().await?.error_for_status()?;
        // 大小校验: 与 cfwidget 元数据不符说明推导的 URL 指向了别的文件, 硬失败防错包
        if file.filesize > 0
            && resp
                .content_length()
                .is_some_and(|len| len != file.filesize)
        {
            bail!(
                "CDN 文件大小 {:?} 与元数据 {} 不符, 放弃下载",
                resp.content_length(),
                file.filesize
            );
        }
        std::fs::create_dir_all(cache_dir)?;
        let jar_path = cache_dir.join(format!("{}-{}.jar", project.id, file.id));
        let tmp = cache_dir.join(format!(".tmp-{}", file.id));
        let mut out = std::fs::File::create(&tmp)?;
        let mut h1 = Sha1::new();
        let mut h512 = Sha512::new();
        let mut size: u64 = 0;
        let mut interrupted = false;
        use tokio_stream::StreamExt;
        let mut stream = resp.bytes_stream();
        loop {
            match stream.next().await {
                Some(Ok(chunk)) => {
                    h1.update(&chunk);
                    h512.update(&chunk);
                    if let Err(e) = out.write_all(&chunk) {
                        let _ = std::fs::remove_file(&tmp);
                        return Err(e.into());
                    }
                    size += chunk.len() as u64;
                }
                Some(Err(e)) => {
                    let _ = std::fs::remove_file(&tmp);
                    return Err(e.into());
                }
                None => break,
            }
            if ctx.interrupted() {
                interrupted = true;
                break;
            }
        }
        drop(out);
        if interrupted {
            let _ = std::fs::remove_file(&tmp);
            bail!(crate::tools::TOOL_INTERRUPTED);
        }
        std::fs::rename(&tmp, &jar_path)?;
        let meta = CfFileMeta {
            mod_id: project.id,
            file_id: file.id,
            filename: file.name.clone(),
            url: cdn,
            sha1: format!("{:x}", h1.finalize()),
            sha512: format!("{:x}", h512.finalize()),
            size,
            depends: read_depends(&jar_path).unwrap_or_default(),
        };
        std::fs::write(&sidecar, serde_json::to_string(&meta)?)?;
        Ok(meta)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(id: u64, kind: &str, versions: &[&str]) -> CfFile {
        CfFile {
            id,
            name: format!("f{id}.jar"),
            kind: kind.into(),
            filesize: 0,
            versions: versions.iter().map(|s| s.to_string()).collect(),
        }
    }

    #[test]
    fn select_file_prefers_release_and_latest() {
        let files = vec![
            file(10, "beta", &["1.21.1", "Fabric", "Client"]),
            file(20, "release", &["1.21.1", "Fabric", "Client", "Server"]),
            file(30, "release", &["1.20.1", "Fabric"]),
            file(40, "release", &["1.21.1", "Forge"]),
        ];
        let sel = select_file(&files, "1.21.1", "fabric").unwrap();
        assert_eq!(sel.id, 20, "应选 1.21.1+Fabric 的 release");
        assert!(select_file(&files, "1.19.2", "fabric").is_none());
        assert!(select_file(&files, "1.21.1", "quilt").is_none());
    }

    #[test]
    fn env_tags_map_to_mrpack_env() {
        let client_only = file(1, "release", &["1.21.1", "Fabric", "Client"]);
        assert_eq!(
            env_from_file(&client_only),
            ("required".into(), "unsupported".into())
        );
        let both = file(2, "release", &["1.21.1", "Fabric", "Client", "Server"]);
        assert_eq!(env_from_file(&both), ("required".into(), "required".into()));
    }

    #[test]
    fn slugify_builds_cf_slug_from_query() {
        assert_eq!(slugify("The Twilight Forest"), "the-twilight-forest");
        assert_eq!(slugify(" 暮色森林 "), "", "中文应得到空串由调用方跳过");
    }

    #[test]
    fn cdn_url_derives_from_file_id() {
        assert_eq!(
            cdn_url(8749067, "a.jar"),
            "https://mediafilez.forgecdn.net/files/8749/67/a.jar"
        );
        assert_eq!(
            cdn_url(8792636, "b.jar"),
            "https://mediafilez.forgecdn.net/files/8792/636/b.jar"
        );
    }

    fn write_jar(path: &Path, entries: &[(&str, &str)]) {
        let f = std::fs::File::create(path).unwrap();
        let mut z = zip::ZipWriter::new(f);
        for (name, content) in entries {
            z.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            std::io::Write::write_all(&mut z, content.as_bytes()).unwrap();
        }
        z.finish().unwrap();
    }

    #[test]
    fn read_depends_parses_fabric_json_and_toml() {
        let dir = std::env::temp_dir().join(format!("rustagent-cf-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let fj = dir.join("fabric-mod.jar");
        write_jar(
            &fj,
            &[(
                "fabric.mod.json",
                r#"{"depends": {"minecraft": "~1.21", "java": ">=17", "fabricloader": ">=0.16", "fabric-api": "*", "twilightforest": "*"}}"#,
            )],
        );
        let deps = read_depends(&fj).unwrap();
        assert_eq!(deps, vec!["fabric-api", "twilightforest"]);

        let tj = dir.join("forge-mod.jar");
        write_jar(
            &tj,
            &[(
                "META-INF/mods.toml",
                "modLoader=\"javafml\"\n[[dependencies.example]]\nmodId=\"forge\"\n[[dependencies.example]]\nmodId=\"geckolib3\"\n",
            )],
        );
        let deps = read_depends(&tj).unwrap();
        assert_eq!(deps, vec!["geckolib3"]);
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
