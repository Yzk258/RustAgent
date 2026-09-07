//! RustAgent 库入口: 组织全部功能模块, 并提供 prelude 集中高频导入。
//! 新增模块时在此登记一条 `pub mod`, 高频公共导入进 prelude。

pub mod agent;
pub mod cli;
pub mod config;
pub mod curseforge;
pub mod database;
pub mod history;
pub mod llm;
pub mod modrinth;
pub mod pipeline;
pub mod tools;
pub mod ui;

/// 只收跨模块反复出现的 (anyhow / serde / json / Arc / atomic), 过度膨胀反而难查。
pub mod prelude {
    pub use anyhow::{anyhow, bail, Context, Result};
    pub use serde::{Deserialize, Serialize};
    pub use serde_json::{json, Value};
    pub use std::sync::atomic::{AtomicBool, Ordering};
    pub use std::sync::Arc;
}
