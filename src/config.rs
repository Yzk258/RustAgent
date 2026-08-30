use anyhow::Context;
use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct Config {
    pub llm: LlmConfig,
    pub output: OutputConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
}

#[derive(Deserialize, Clone)]
pub struct LlmConfig {
    pub base_url: String,
    pub api_key: String,
    pub model: String,
    #[serde(default = "default_context_length")]
    pub context_length: u64,
    #[serde(default)]
    pub price_input_per_m: f64,
    #[serde(default)]
    pub price_output_per_m: f64,
    #[serde(default)]
    pub token_budget: u64,
}

fn default_context_length() -> u64 {
    32768
}

#[derive(Deserialize, Clone)]
pub struct OutputConfig {
    pub download_dir: String,
}

#[derive(Deserialize, Clone, Default)]
pub struct DatabaseConfig {
    pub path: Option<String>,
}

impl Config {
    pub fn db_path(&self) -> String {
        self.database
            .path
            .clone()
            .unwrap_or_else(|| "userdata.json".to_string())
    }
}

pub fn load(path: &str) -> anyhow::Result<Config> {
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("找不到 {path}, 请复制 config.example.toml 为 config.toml 并填写 api_key"))?;
    let cfg: Config = toml::from_str(&text).context("config.toml 格式错误")?;
    Ok(cfg)
}
