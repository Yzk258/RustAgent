use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::io::Write;
use std::time::Duration;
use std::{fs::File, process};

const API_BASE: &str = "https://api.modrinth.com/v2";
const FABRIC_META: &str = "https://meta.fabricmc.net/v2/versions/loader";
const USER_AGENT: &str = "RustAgent/0.1 (tsinghua rust course project)";

fn client() -> Result<reqwest::blocking::Client, Box<dyn std::error::Error>> {
    Ok(reqwest::blocking::Client::builder()
        .user_agent(USER_AGENT)
        .timeout(Duration::from_secs(30))
        .build()?)
}

// ---------- 搜索模式 ----------

#[derive(Deserialize)]
struct SearchResponse {
    total_hits: u32,
    hits: Vec<Hit>,
}

#[derive(Deserialize)]
struct Hit {
    slug: String,
    title: String,
    description: String,
    author: String,
    downloads: u64,
    #[serde(default)]
    display_categories: Vec<String>,
    #[serde(default)]
    versions: Vec<String>,
}

fn search(client: &reqwest::blocking::Client, query: &str) -> Result<(), Box<dyn std::error::Error>> {
    let url = format!("{API_BASE}/search");
    let resp: SearchResponse = client
        .get(&url)
        .query(&[("limit", "5"), ("query", query)])
        .send()?
        .error_for_status()?
        .json()?;

    println!("查询: {query}");
    println!("命中总数: {}", resp.total_hits);
    println!("---");

    for hit in &resp.hits {
        println!("[{}] {}", hit.title, hit.slug);
        println!("  作者: {} | 下载量: {}", hit.author, hit.downloads);
        println!("  简介: {}", hit.description);
        println!("  分类: {}", hit.display_categories.join(", "));
        let latest: Vec<String> = hit.versions.iter().rev().take(4).cloned().collect();
        println!(
            "  支持版本: 共{}个, 最近: {}",
            hit.versions.len(),
            latest.join(", ")
        );
        println!("---");
    }
    Ok(())
}

// ---------- 组包模式 ----------

#[derive(Deserialize)]
struct ModVersion {
    version_number: String,
    #[serde(default)]
    files: Vec<VersionFile>,
}

#[derive(Deserialize)]
struct VersionFile {
    filename: String,
    url: String,
    primary: bool,
    size: u64,
    hashes: Hashes,
}

#[derive(Deserialize)]
struct Hashes {
    sha1: String,
    sha512: String,
}

#[derive(Deserialize)]
struct FabricLoaderEntry {
    version: String,
    stable: bool,
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

fn pack(client: &reqwest::blocking::Client) -> Result<(), Box<dyn std::error::Error>> {
    let game_version = "1.21.1";
    let loader = "fabric";
    let mods = ["sodium", "fabric-api"];

    let mut files = Vec::new();
    for slug in mods {
        let url = format!("{API_BASE}/project/{slug}/version");
        let versions: Vec<ModVersion> = client
            .get(&url)
            .query(&[
                ("game_versions", format!("[\"{game_version}\"]")),
                ("loaders", format!("[\"{loader}\"]")),
            ])
            .send()?
            .error_for_status()?
            .json()?;
        let v = versions
            .first()
            .ok_or(format!("{slug} 没有 {game_version}/{loader} 的版本"))?;
        let f = v
            .files
            .iter()
            .find(|f| f.primary)
            .unwrap_or(&v.files[0]);
        println!("{} -> {} ({})", slug, v.version_number, f.filename);
        files.push(IndexFile {
            path: format!("mods/{}", f.filename),
            hashes: IndexHashes {
                sha1: f.hashes.sha1.clone(),
                sha512: f.hashes.sha512.clone(),
            },
            env: Env {
                client: "required".into(),
                server: "required".into(),
            },
            downloads: vec![f.url.clone()],
            file_size: f.size,
        });
    }

    let loader_version = match client.get(FABRIC_META).send() {
        Ok(resp) => {
            let entries: Vec<FabricLoaderEntry> = resp.error_for_status()?.json()?;
            entries
                .into_iter()
                .find(|e| e.stable)
                .map(|e| e.version)
                .unwrap_or_else(|| "0.16.14".into())
        }
        Err(_) => {
            println!("fabric meta 不可达, 使用内置版本 0.16.14");
            "0.16.14".into()
        }
    };
    println!("fabric-loader: {loader_version}");

    let mut dependencies = BTreeMap::new();
    dependencies.insert("minecraft".to_string(), game_version.to_string());
    dependencies.insert("fabric-loader".to_string(), loader_version);

    let index = PackIndex {
        format_version: 1,
        game: "minecraft".into(),
        version_id: format!("rustagent-test-{game_version}"),
        name: "RustAgent 测试整合包".into(),
        summary: "RustAgent 生成的最小验证整合包".into(),
        files,
        dependencies,
    };
    let json = serde_json::to_string_pretty(&index)?;

    let out_path = "RustAgent-test.mrpack";
    let file = File::create(out_path)?;
    let mut zip = zip::ZipWriter::new(file);
    zip.start_file("modrinth.index.json", zip::write::SimpleFileOptions::default())?;
    zip.write_all(json.as_bytes())?;
    zip.finish()?;
    println!("已生成 {out_path} ({} 个 mod)", index.files.len());
    Ok(())
}

fn main() {
    let client = match client() {
        Ok(c) => c,
        Err(e) => {
            eprintln!("初始化 HTTP 客户端失败: {e}");
            process::exit(1);
        }
    };
    let arg = std::env::args().nth(1).unwrap_or_default();
    let result = if arg == "pack" {
        pack(&client)
    } else {
        let query = if arg.is_empty() { "create".to_string() } else { arg };
        search(&client, &query)
    };
    if let Err(e) = result {
        eprintln!("执行失败: {e}");
        process::exit(1);
    }
}
