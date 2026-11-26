//! ACP 官方登记表（设计 73 第四节第 1 条）：取、缓存、读。
//!
//! - 缓存在缓存目录的 `registry.json`；比一天（[`MAX_AGE`]）新的直接用，过期了才重取；取不到、取到的读不懂的照旧用缓存（再旧
//!   也用，记一行日志）；没有缓存又取不到的报错。
//! - 网址照 [`REGISTRY_URL`]；环境变量 [`REGISTRY_ENV`] 可以换掉它（只给测试和离线镜像用，不是配置项）。
//! - 读出来每个 agent 一项（[`Entry`]）：编号、名字、版本、网址和 `distribution` 原样（怎么装由 [`crate::install`] 看）。

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use serde_json::Value;

/// 官方登记表的网址。
pub const REGISTRY_URL: &str =
    "https://cdn.agentclientprotocol.com/registry/v1/latest/registry.json";

/// 换登记表网址的环境变量。
pub const REGISTRY_ENV: &str = "MIYU_ACP_REGISTRY_URL";

/// 缓存多久算过期。
pub const MAX_AGE: Duration = Duration::from_hours(24);

/// 登记表至多多大（2026-11-26 是 76 KB）。
const LIMIT: u64 = 16 * 1024 * 1024;

/// 登记表里的一个 agent。
#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    /// 编号（`claude-acp`、`gemini`）。
    pub id: String,
    /// 给人看的名字。
    pub name: String,
    /// 版本。
    pub version: String,
    /// 网站（没有的用仓库地址）：要登录时告诉主人去哪看。
    pub website: Option<String>,
    /// `distribution` 原样。
    pub distribution: Value,
}

/// 读出来的登记表。
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Catalog {
    /// 每个 agent 一项，照登记表的先后。
    pub agents: Vec<Entry>,
}

impl Catalog {
    /// 编号是 `id` 的那一项。
    #[must_use]
    pub fn find(&self, id: &str) -> Option<&Entry> {
        self.agents.iter().find(|entry| entry.id == id)
    }
}

/// 读登记表的字节：要有 `agents` 数组；每项要有字的 `id`、对象的 `distribution`，缺了的那一项跳过。
///
/// # Errors
///
/// 不是 JSON、没有 `agents` 数组：英文的一句原因。
pub fn parse(bytes: &[u8]) -> Result<Catalog, String> {
    let value: Value = serde_json::from_slice(bytes)
        .map_err(|error| format!("the registry is not JSON: {error}"))?;
    let agents = value
        .get("agents")
        .and_then(Value::as_array)
        .ok_or("the registry has no agents list")?;
    let text = |item: &Value, key: &str| item.get(key).and_then(Value::as_str).map(str::to_string);
    Ok(Catalog {
        agents: agents
            .iter()
            .filter_map(|item| {
                let id = text(item, "id")?;
                let distribution = item.get("distribution").filter(|d| d.is_object())?.clone();
                Some(Entry {
                    name: text(item, "name").unwrap_or_else(|| id.clone()),
                    version: text(item, "version").unwrap_or_default(),
                    website: text(item, "website").or_else(|| text(item, "repository")),
                    id,
                    distribution,
                })
            })
            .collect(),
    })
}

/// 登记表的网址：[`REGISTRY_ENV`] 给了非空的照它，别的照 [`REGISTRY_URL`]。
#[must_use]
pub fn url() -> String {
    std::env::var(REGISTRY_ENV)
        .ok()
        .filter(|url| !url.is_empty())
        .unwrap_or_else(|| REGISTRY_URL.to_string())
}

/// 缓存文件的位置。
#[must_use]
pub fn cache_file(cache_dir: &Path) -> PathBuf {
    cache_dir.join("registry.json")
}

/// 取登记表：缓存比 [`MAX_AGE`] 新的直接用，否则用 `fetch` 取 `url`、写进缓存；取不到的用旧缓存。`now` 是现在。
///
/// # Errors
///
/// 没有能用的缓存、又取不到：英文的一句原因。
pub fn load(
    cache_dir: &Path,
    url: &str,
    now: SystemTime,
    fetch: impl FnOnce(&str) -> Result<Vec<u8>, String>,
) -> Result<Catalog, String> {
    let file = cache_file(cache_dir);
    let cached = std::fs::read(&file).ok().and_then(|bytes| {
        let age = std::fs::metadata(&file)
            .and_then(|meta| meta.modified())
            .ok()
            .and_then(|modified| now.duration_since(modified).ok());
        parse(&bytes).ok().map(|catalog| (catalog, age))
    });
    if let Some((catalog, Some(age))) = &cached
        && *age < MAX_AGE
    {
        return Ok(catalog.clone());
    }
    let fetched = fetch(url).and_then(|bytes| parse(&bytes).map(|catalog| (catalog, bytes)));
    match (fetched, cached) {
        (Ok((catalog, bytes)), _) => {
            if let Err(error) = store(&file, &bytes) {
                eprintln!(
                    "the registry could not be cached at {}: {error}",
                    file.display()
                );
            }
            Ok(catalog)
        }
        (Err(why), Some((catalog, _))) => {
            eprintln!("refreshing the ACP registry failed ({why}); using the cached copy");
            Ok(catalog)
        }
        (Err(why), None) => Err(format!("the ACP registry could not be fetched: {why}")),
    }
}

/// 照真的网络取：[`load`] 加上 [`crate::fetch::get`]。
///
/// # Errors
///
/// 同 [`load`]。
pub fn load_now(cache_dir: &Path) -> Result<Catalog, String> {
    load(cache_dir, &url(), SystemTime::now(), |url| {
        crate::fetch::get(url, LIMIT)
    })
}

/// 先写临时文件再改名，写一半不留坏的缓存。
fn store(file: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(parent) = file.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let partial = file.with_extension(format!("partial-{}", crate::workspace::unique()));
    std::fs::write(&partial, bytes)?;
    std::fs::rename(&partial, file).inspect_err(|_| drop(std::fs::remove_file(&partial)))
}

#[cfg(test)]
mod tests;
