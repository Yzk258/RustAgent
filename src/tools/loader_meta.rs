//! 加载器版本与 .mrpack 依赖键元数据: 四种加载器的最新稳定版获取 + 版本排序工具。

use super::*;

impl super::ToolRegistry {
    /// 获取加载器在指定 MC 版本下的最新稳定版本号, 四种加载器全部支持。
    pub async fn loader_version(&self, loader: &str, game_version: &str) -> Result<String> {
        #[derive(Deserialize)]
        struct Entry {
            version: String,
            #[serde(default)]
            stable: bool,
        }
        match loader {
            "fabric" | "quilt" => {
                let meta = if loader == "fabric" {
                    "https://meta.fabricmc.net/v2/versions/loader"
                } else {
                    "https://meta.quiltmc.org/v3/versions/loader"
                };
                let entries: Vec<Entry> = self
                    .modrinth
                    .http()
                    .get(meta)
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;
                // fabric meta 有 stable 标记取首个稳定版; quilt meta v3 条目无 stable 字段,
                // 改为取无预发布后缀 (-beta.N) 的最新版本
                let found = if loader == "fabric" {
                    entries.into_iter().find(|e| e.stable).map(|e| e.version)
                } else {
                    entries
                        .iter()
                        .map(|e| e.version.as_str())
                        .filter(|v| !v.contains('-'))
                        .max_by_key(|v| version_key(v))
                        .map(str::to_string)
                };
                found.ok_or_else(|| anyhow::anyhow!("{loader} meta 无稳定版本"))
            }
            "forge" => {
                #[derive(Deserialize)]
                struct Promos {
                    promos: BTreeMap<String, String>,
                }
                let p: Promos = self
                    .modrinth
                    .http()
                    .get("https://files.minecraftforge.net/maven/net/minecraftforge/forge/promotions_slim.json")
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;
                p.promos
                    .get(&format!("{game_version}-recommended"))
                    .or_else(|| p.promos.get(&format!("{game_version}-latest")))
                    .cloned()
                    .ok_or_else(|| anyhow::anyhow!("forge 无 {game_version} 的可用版本"))
            }
            "neoforge" => {
                #[derive(Deserialize)]
                struct Releases {
                    versions: Vec<String>,
                }
                let r: Releases = self
                    .modrinth
                    .http()
                    .get("https://maven.neoforged.net/api/maven/versions/releases/net/neoforged/neoforge")
                    .send()
                    .await?
                    .error_for_status()?
                    .json()
                    .await?;
                pick_neoforge_version(game_version, &r.versions)
                    .ok_or_else(|| anyhow::anyhow!("neoforge 无 {game_version} 的可用版本"))
            }
            _ => bail!("不支持的加载器: {loader}"),
        }
    }
}

/// 各加载器在 .mrpack dependencies 中的键名 (Modrinth 官方格式, 已用官方生成的
/// forge/neoforge 整合包实测: forge/neoforge 用裸构建号, 如 "forge": "47.4.20")。
pub(super) fn loader_dep_key(loader: &str) -> Result<&'static str> {
    match loader {
        "fabric" => Ok("fabric-loader"),
        "quilt" => Ok("quilt-loader"),
        "forge" => Ok("forge"),
        "neoforge" => Ok("neoforge"),
        _ => bail!("不支持的加载器: {loader} (可选: fabric / forge / neoforge / quilt)"),
    }
}

/// 版本号按数字逐段比较的排序键 ("21.1.9" < "21.1.77" 按数值而非字典序)。
fn version_key(v: &str) -> Vec<u64> {
    v.split('.')
        .map(|s| s.parse::<u64>().unwrap_or(0))
        .collect()
}

/// 从 neoforge maven 版本列表里挑适配 MC 版本的最新稳定构建。
/// NeoForge 版本号跟随 MC 主次版本 (1.21.1 → 21.1.x); 1.20.1 例外 ——
/// 当时 NeoForge fork 自 Forge, 沿用 Forge 的 47.1.x 编号。
fn pick_neoforge_version(game_version: &str, versions: &[String]) -> Option<String> {
    let mut seg = game_version.split('.');
    let _major = seg.next()?;
    let mid = seg.next()?;
    let minor = seg.next().unwrap_or("0");
    let prefix = format!("{mid}.{minor}.");
    let legacy = game_version == "1.20.1";
    versions
        .iter()
        .filter(|v| !v.contains('-'))
        .filter(|v| v.starts_with(&prefix) || (legacy && v.starts_with("47.1.")))
        .max_by_key(|v| version_key(v))
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loader_dep_key_maps_supported_rejects_unknown() {
        assert_eq!(loader_dep_key("fabric").unwrap(), "fabric-loader");
        assert_eq!(loader_dep_key("quilt").unwrap(), "quilt-loader");
        assert_eq!(loader_dep_key("forge").unwrap(), "forge");
        assert_eq!(loader_dep_key("neoforge").unwrap(), "neoforge");
        assert!(loader_dep_key("optifine").is_err());
    }

    #[test]
    fn picks_neoforge_version_matching_mc() {
        let vs: Vec<String> = [
            "47.1.104",
            "20.4.237",
            "21.0.1",
            "21.1.9",
            "21.1.77",
            "21.2.0-beta.1",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        assert_eq!(pick_neoforge_version("1.21.1", &vs).unwrap(), "21.1.77");
        assert_eq!(pick_neoforge_version("1.20.1", &vs).unwrap(), "47.1.104");
        assert_eq!(pick_neoforge_version("1.19.2", &vs), None);
        assert_eq!(pick_neoforge_version("bad", &vs), None);
    }
}
