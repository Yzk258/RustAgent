use anyhow::{bail, Context};
use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct Config {
    pub llm: LlmConfig,
    pub output: OutputConfig,
    #[serde(default)]
    pub database: DatabaseConfig,
    #[serde(default)]
    pub ui: UiConfig,
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
    /// 一轮对话最多工具调用次数 (超过即中止), 复杂组包需求搜索多, 默认 16
    #[serde(default = "default_max_tool_iterations")]
    pub max_tool_iterations: u32,
    /// 思考模式透传: auto/省略 = 不发送参数; 其余按 key=value (逗号分隔多组)
    /// 原样并入请求体, 如 enable_thinking=false / reasoning_effort=medium
    #[serde(default)]
    pub thinking: Option<String>,
}

fn default_context_length() -> u64 {
    32768
}

fn default_max_tool_iterations() -> u32 {
    16
}

#[derive(Deserialize, Clone)]
pub struct OutputConfig {
    pub download_dir: String,
}

#[derive(Deserialize, Clone, Default)]
pub struct DatabaseConfig {
    pub path: Option<String>,
}

/// Web UI 配置段: 目前只有端口, 后续 UI 相关选项 (主题/自动打开浏览器等) 在此扩展
#[derive(Deserialize, Clone)]
pub struct UiConfig {
    #[serde(default = "default_ui_port")]
    pub port: u16,
}

impl Default for UiConfig {
    fn default() -> Self {
        Self {
            port: default_ui_port(),
        }
    }
}

fn default_ui_port() -> u16 {
    1780
}

impl Config {
    pub fn db_path(&self) -> String {
        self.database
            .path
            .clone()
            .unwrap_or_else(|| "userdata/userdata.db".to_string())
    }

    /// 用户数据根目录 (db 文件所在目录), 会话与整合包目录由它派生
    pub fn data_dir(&self) -> String {
        std::path::Path::new(&self.db_path())
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| ".".to_string())
    }

    pub fn validate(&self) -> anyhow::Result<()> {
        let url = reqwest::Url::parse(&self.llm.base_url)
            .with_context(|| format!("LLM base_url 不是有效 URL: {}", self.llm.base_url))?;
        if !matches!(url.scheme(), "http" | "https") {
            bail!("LLM base_url 只支持 http 或 https");
        }
        if self.llm.context_length < 256 {
            bail!("context_length 不能小于 256");
        }
        if self.llm.price_input_per_m < 0.0 || self.llm.price_output_per_m < 0.0 {
            bail!("token 单价不能为负数");
        }
        if self.llm.max_tool_iterations == 0 {
            bail!("max_tool_iterations 必须大于 0");
        }
        if let Some(t) = &self.llm.thinking {
            let ok = t.split(',').all(|seg| {
                let seg = seg.trim();
                seg.is_empty() || seg.eq_ignore_ascii_case("auto") || seg.contains('=')
            });
            if !ok {
                bail!("llm.thinking 应为 key=value (逗号分隔多组) 或 auto, 例如 enable_thinking=false");
            }
        }
        if self.output.download_dir.trim().is_empty() {
            bail!("output.download_dir 不能为空");
        }
        Ok(())
    }
}

pub fn load(path: &str) -> anyhow::Result<Config> {
    let text = std::fs::read_to_string(path).with_context(|| {
        format!("找不到 {path}, 请复制 config.example.toml 为 config.toml 并填写 api_key")
    })?;
    let cfg: Config = toml::from_str(&text).context("config.toml 格式错误")?;
    cfg.validate().context("配置校验失败")?;
    Ok(cfg)
}
