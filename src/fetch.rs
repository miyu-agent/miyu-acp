//! 取一个网址的内容：登记表、agent 的压缩包。`https://`、`http://` 走 [`ureq`]（纯 Rust 的 HTTPS，照系统的代理设置），
//! `file://` 直接读本机文件（测试、离线镜像用）。下载压缩包时边写文件边算 SHA-256，好和登记表的 `sha256` 对。

use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use sha2::{Digest, Sha256};

/// 连上、读一段都至多等多久。
const TIMEOUT: Duration = Duration::from_secs(30);

/// 打开 `url` 读：交读的一头。
fn open(url: &str) -> Result<Box<dyn Read + Send>, String> {
    if let Some(path) = url.strip_prefix("file://") {
        let file =
            std::fs::File::open(path).map_err(|error| format!("cannot read {path}: {error}"))?;
        return Ok(Box::new(file));
    }
    if !url.starts_with("https://") && !url.starts_with("http://") {
        return Err(format!("not an http(s) or file URL: {url}"));
    }
    // 只限「卡住多久」不限总时长：压缩包可能几百 MB（ureq 3 默认一项超时都没有，见 miyu-ocr 审计 #133）。
    let config = ureq::Agent::config_builder()
        .timeout_resolve(Some(TIMEOUT))
        .timeout_connect(Some(TIMEOUT))
        .timeout_send_request(Some(TIMEOUT))
        .timeout_recv_response(Some(TIMEOUT))
        .timeout_per_read(Some(TIMEOUT))
        .build();
    let agent = ureq::Agent::new_with_config(config);
    let response = agent
        .get(url)
        .header(
            "User-Agent",
            concat!("miyu-acp/", env!("CARGO_PKG_VERSION")),
        )
        .call()
        .map_err(|error| format!("cannot download {url}: {error}"))?;
    Ok(Box::new(response.into_body().into_reader()))
}

/// 读 `url` 的全部内容，至多 `limit` 字节。
///
/// # Errors
///
/// 取不到、超过上限：英文的一句原因。
pub fn get(url: &str, limit: u64) -> Result<Vec<u8>, String> {
    let mut bytes = Vec::new();
    open(url)?
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)
        .map_err(|error| format!("reading {url} failed: {error}"))?;
    if bytes.len() as u64 > limit {
        return Err(format!("{url} is larger than {limit} bytes"));
    }
    Ok(bytes)
}

/// 把 `url` 下载到 `dest`（至多 `limit` 字节），交内容的 SHA-256（小写十六进制）。出错时 `dest` 删掉。
///
/// # Errors
///
/// 取不到、写不了、超过上限：英文的一句原因。
pub fn download(url: &str, dest: &Path, limit: u64) -> Result<String, String> {
    let result = copy(url, dest, limit);
    if result.is_err() {
        drop(std::fs::remove_file(dest));
    }
    result
}

fn copy(url: &str, dest: &Path, limit: u64) -> Result<String, String> {
    let mut reader = open(url)?;
    let mut file = std::fs::File::create(dest)
        .map_err(|error| format!("cannot write {}: {error}", dest.display()))?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 256 * 1024];
    let mut total: u64 = 0;
    loop {
        let read = reader
            .read(&mut buffer)
            .map_err(|error| format!("downloading {url} failed: {error}"))?;
        if read == 0 {
            break;
        }
        total += read as u64;
        if total > limit {
            return Err(format!("{url} is larger than {limit} bytes"));
        }
        hasher.update(&buffer[..read]);
        file.write_all(&buffer[..read])
            .map_err(|error| format!("cannot write {}: {error}", dest.display()))?;
    }
    file.sync_all()
        .map_err(|error| format!("cannot write {}: {error}", dest.display()))?;
    Ok(crate::workspace::hex(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_urls_are_read_and_hashed_and_limits_hold() {
        let dir =
            std::env::temp_dir().join(format!("miyu-acp-fetch-{}", crate::workspace::unique()));
        std::fs::create_dir_all(&dir).unwrap();
        let source = dir.join("a.txt");
        std::fs::write(&source, "hello").unwrap();
        let url = format!("file://{}", source.display());
        assert_eq!(get(&url, 5).unwrap(), b"hello");
        assert!(get(&url, 4).unwrap_err().contains("larger"));
        let dest = dir.join("b");
        assert_eq!(
            download(&url, &dest, 100).unwrap(),
            "2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824"
        );
        assert!(download(&url, &dest, 3).is_err());
        assert!(!dest.exists(), "出错时删掉");
        assert!(get("ftp://x", 10).unwrap_err().contains("not an http"));
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
