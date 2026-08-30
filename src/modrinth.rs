use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const API_BASE: &str = "https://api.modrinth.com/v2";
const USER_AGENT: &str = "RustAgent/0.1 (tsinghua rust course project)";

#[derive(Deserialize)]
pub struct SearchResponse {
    pub total_hits: u32,
    pub hits: Vec<Hit>,
}

#[derive(Serialize, Deserialize, Clone)]
pub struct Hit {
    pub slug: String,
    pub title: String,
    pub description: String,
    pub author: String,
    pub downloads: u64,
    #[serde(default)]
    pub display_categories: Vec<String>,
    #[serde(default)]
    pub versions: Vec<String>,
}

#[derive(Deserialize)]
pub struct Project {
    pub slug: String,
    pub client_side: String,
    pub server_side: String,
    #[serde(default)]
    pub categories: Vec<String>,
}

#[derive(Deserialize)]
pub struct ModVersion {
    pub version_number: String,
    #[serde(default)]
    pub dependencies: Vec<Dependency>,
    #[serde(default)]
    pub files: Vec<VersionFile>,
}

#[derive(Deserialize)]
pub struct Dependency {
    pub dependency_type: String,
    #[serde(default)]
    pub project_id: Option<String>,
}

#[derive(Deserialize, Clone)]
pub struct VersionFile {
    pub filename: String,
    pub url: String,
    pub primary: bool,
    pub size: u64,
    pub hashes: Hashes,
}

#[derive(Deserialize, Clone)]
pub struct Hashes {
    pub sha1: String,
    pub sha512: String,
}

pub struct ModrinthClient {
    http: reqwest::Client,
}

impl ModrinthClient {
    pub fn new() -> Result<Self> {
        let http = reqwest::Client::builder()
            .user_agent(USER_AGENT)
            .build()
            .context("初始化 Modrinth 客户端失败")?;
        Ok(Self { http })
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    pub async fn search(
        &self,
        query: &str,
        facets: Option<Vec<Vec<String>>>,
        limit: u32,
        index: &str,
    ) -> Result<SearchResponse> {
        let mut params: Vec<(&str, String)> = vec![
            ("query", query.to_string()),
            ("limit", limit.to_string()),
            ("index", index.to_string()),
        ];
        if let Some(f) = &facets {
            let json = serde_json::to_string(f)?;
            params.push(("facets", json));
        }
        Ok(self
            .http
            .get(format!("{API_BASE}/search"))
            .query(&params)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?)
    }

    pub async fn project(&self, slug: &str) -> Result<Project> {
        self.http
            .get(format!("{API_BASE}/project/{slug}"))
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("获取 mod {slug} 详情失败"))?
            .json()
            .await
            .context("解析 mod 详情失败")
    }

    pub async fn versions(
        &self,
        slug: &str,
        game_version: &str,
        loader: &str,
    ) -> Result<Vec<ModVersion>> {
        Ok(self
            .http
            .get(format!("{API_BASE}/project/{slug}/version"))
            .query(&[
                ("game_versions", format!("[\"{game_version}\"]")),
                ("loaders", format!("[\"{loader}\"]")),
            ])
            .send()
            .await?
            .error_for_status()
            .with_context(|| format!("获取 mod {slug} 版本失败"))?
            .json()
            .await?)
    }
}
