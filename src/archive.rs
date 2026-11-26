//! 解开登记表里 `binary` 分发的下载（设计 73 第四节第 2 条）：照网址的结尾认 `.zip`、`.tar.gz`（`.tgz`）、`.tar.bz2`，别的当
//! 裸程序（下载下来的就是 `cmd` 那个程序本身，2026-11-26 登记表里有十份这样的）。压缩包里的路径不许跑出目标目录：zip 照
//! `enclosed_name` 认，tar 照 `unpack_in`（它自己挡 `..` 和绝对路径）。

use std::fs::File;
use std::io::BufReader;
use std::path::Path;

/// 下载的是哪一种。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// `.zip`。
    Zip,
    /// `.tar.gz`、`.tgz`。
    TarGz,
    /// `.tar.bz2`。
    TarBz2,
    /// 不是压缩包：下载下来的就是程序。
    Raw,
}

impl Kind {
    /// 照网址认：只看路径部分的结尾（`?` 以后的查询不算），不分大小写。
    #[must_use]
    // 已经整个转成小写再比，clippy 担心的大小写不是问题；`.tar.gz` 这种两段的扩展名也没法照 `Path::extension` 比。
    #[allow(clippy::case_sensitive_file_extension_comparisons)]
    pub fn of(url: &str) -> Kind {
        let path = url
            .split(['?', '#'])
            .next()
            .unwrap_or(url)
            .to_ascii_lowercase();
        if path.ends_with(".zip") {
            Kind::Zip
        } else if path.ends_with(".tar.gz") || path.ends_with(".tgz") {
            Kind::TarGz
        } else if path.ends_with(".tar.bz2") {
            Kind::TarBz2
        } else {
            Kind::Raw
        }
    }
}

/// 把下载下来的 `file` 照 `kind` 解进 `dest`（建好的空目录）。裸程序存成 `dest/<cmd 的文件名>`、加上执行权限。
///
/// # Errors
///
/// 读不了、写不了、压缩包坏了、里面有跑出目录的路径：英文的一句原因。
pub fn extract(file: &Path, kind: Kind, dest: &Path, cmd: &str) -> Result<(), String> {
    let failed =
        |error: &dyn std::fmt::Display| format!("cannot unpack {}: {error}", file.display());
    match kind {
        Kind::Zip => unzip(file, dest).map_err(|error| failed(&error)),
        Kind::TarGz => {
            let reader = BufReader::new(File::open(file).map_err(|error| failed(&error))?);
            untar(flate2::read::GzDecoder::new(reader), dest).map_err(|error| failed(&error))
        }
        Kind::TarBz2 => {
            let reader = BufReader::new(File::open(file).map_err(|error| failed(&error))?);
            untar(bzip2::read::BzDecoder::new(reader), dest).map_err(|error| failed(&error))
        }
        Kind::Raw => {
            let name = Path::new(cmd)
                .file_name()
                .ok_or_else(|| format!("the command {cmd:?} names no file"))?;
            let target = dest.join(name);
            std::fs::copy(file, &target).map_err(|error| failed(&error))?;
            make_executable(&target).map_err(|error| failed(&error))
        }
    }
}

fn untar(reader: impl std::io::Read, dest: &Path) -> std::io::Result<()> {
    let mut archive = tar::Archive::new(reader);
    archive.set_preserve_permissions(true);
    for entry in archive.entries()? {
        let mut entry = entry?;
        if !entry.unpack_in(dest)? {
            return Err(std::io::Error::other(format!(
                "the archive holds a path outside its folder: {}",
                entry.path()?.display()
            )));
        }
    }
    Ok(())
}

fn unzip(file: &Path, dest: &Path) -> std::io::Result<()> {
    let mut archive =
        zip::ZipArchive::new(BufReader::new(File::open(file)?)).map_err(std::io::Error::other)?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(std::io::Error::other)?;
        let Some(relative) = entry.enclosed_name() else {
            return Err(std::io::Error::other(format!(
                "the archive holds a path outside its folder: {}",
                entry.name().unwrap_or_default()
            )));
        };
        let target = dest.join(relative);
        if entry.is_dir() {
            std::fs::create_dir_all(&target)?;
            continue;
        }
        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let mut out = File::create(&target)?;
        std::io::copy(&mut entry, &mut out)?;
        if let Some(mode) = entry.unix_mode() {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(mode & 0o777))?;
        }
    }
    Ok(())
}

/// 给 `path` 加上所有人的执行权限（读写照旧）。
///
/// # Errors
///
/// 读不了、改不了权限：原样交。
pub fn make_executable(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = std::fs::metadata(path)?.permissions().mode();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode | 0o755))
}

#[cfg(test)]
mod tests;
