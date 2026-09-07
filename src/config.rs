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

/// 写入键值并保留旧值的行内注释 (toml_edit 直接赋新值会丢掉原 decor)
fn set_keep_decor(table: &mut toml_edit::Table, key: &str, v: toml_edit::Value) {
    let mut item = toml_edit::Item::Value(v);
    if let Some(old) = table.get(key).and_then(|i| i.as_value()) {
        let decor = old.decor().clone();
        if let Some(new) = item.as_value_mut() {
            *new.decor_mut() = decor;
        }
    }
    table.insert(key, item);
}

/// 把 [llm] 段写回 config.toml (设置窗口保存时调用)。
/// 用 toml_edit 做文档级编辑, 保留用户文件里的注释与排版。
/// thinking 为 None 时删除该键 (回到不发送任何参数的默认行为)。
pub fn save_llm(path: &str, llm: &LlmConfig) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(path).with_context(|| format!("读取 {path} 失败"))?;
    let mut doc = text
        .parse::<toml_edit::DocumentMut>()
        .with_context(|| format!("{path} 不是合法 TOML, 无法写回设置"))?;
    let item = doc["llm"].or_insert(toml_edit::Item::Table(toml_edit::Table::new()));
    let Some(table) = item.as_table_mut() else {
        bail!("[llm] 段不是 TOML 表, 无法写回设置");
    };
    set_keep_decor(table, "base_url", llm.base_url.clone().into());
    set_keep_decor(table, "api_key", llm.api_key.clone().into());
    set_keep_decor(table, "model", llm.model.clone().into());
    set_keep_decor(table, "context_length", (llm.context_length as i64).into());
    set_keep_decor(table, "price_input_per_m", llm.price_input_per_m.into());
    set_keep_decor(table, "price_output_per_m", llm.price_output_per_m.into());
    set_keep_decor(table, "token_budget", (llm.token_budget as i64).into());
    set_keep_decor(
        table,
        "max_tool_iterations",
        (llm.max_tool_iterations as i64).into(),
    );
    match &llm.thinking {
        Some(s) => set_keep_decor(table, "thinking", s.clone().into()),
        None => {
            table.remove("thinking");
        }
    }
    std::fs::write(path, doc.to_string()).with_context(|| format!("写入 {path} 失败"))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn save_llm_updates_values_and_keeps_comments() {
        let dir = std::env::temp_dir().join(format!("rustagent-cfg-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("config.toml");
        std::fs::write(
            &path,
            "# 顶部注释\n[llm]\nmodel = \"a\" # 行内注释\napi_key = \"k\"\nbase_url = \"http://x/v1\"\n\n[output]\ndownload_dir = \"./dl\"\n",
        )
        .unwrap();
        let cfg = LlmConfig {
            base_url: "http://y/v1".into(),
            api_key: "k2".into(),
            model: "b".into(),
            context_length: 999,
            price_input_per_m: 0.1,
            price_output_per_m: 0.2,
            token_budget: 5,
            max_tool_iterations: 7,
            thinking: Some("reasoning_effort=low".into()),
        };
        save_llm(path.to_str().unwrap(), &cfg).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(text.contains("# 顶部注释"), "段外注释应保留");
        assert!(text.contains("# 行内注释"), "行内注释应保留");
        let cfg2: Config = toml::from_str(&text).unwrap();
        assert_eq!(cfg2.llm.model, "b");
        assert_eq!(cfg2.llm.context_length, 999);
        assert_eq!(cfg2.llm.max_tool_iterations, 7);
        assert_eq!(cfg2.llm.thinking.as_deref(), Some("reasoning_effort=low"));

        // 清除 thinking -> 键应被移除
        let cfg = LlmConfig {
            thinking: None,
            ..cfg
        };
        save_llm(path.to_str().unwrap(), &cfg).unwrap();
        let reloaded = std::fs::read_to_string(&path).unwrap();
        let cfg3: Config = toml::from_str(&reloaded).unwrap();
        assert!(cfg3.llm.thinking.is_none());
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
