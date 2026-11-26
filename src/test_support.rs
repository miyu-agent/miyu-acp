//! 单元测试共用的夹具：临时目录、造压缩包。

use std::io::Write;
use std::path::PathBuf;

/// 新建一个空的临时目录（名字带 `name`）。
pub fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("miyu-acp-{name}-{}", crate::workspace::unique()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// 一份文件：路径、内容、权限。
pub type Entry<'a> = (&'a str, &'a [u8], u32);

/// 造一个 tar 包（未压缩的字节）。
fn tar(files: &[Entry<'_>]) -> Vec<u8> {
    let mut builder = tar::Builder::new(Vec::new());
    for (path, data, mode) in files {
        let mut header = tar::Header::new_gnu();
        header.set_size(data.len() as u64);
        header.set_mode(*mode);
        header.set_cksum();
        builder.append_data(&mut header, path, *data).unwrap();
    }
    builder.into_inner().unwrap()
}

/// 造一个 `.tar.gz`。
pub fn tar_gz(files: &[Entry<'_>]) -> Vec<u8> {
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&tar(files)).unwrap();
    encoder.finish().unwrap()
}

/// 造一个 `.tar.bz2`。
pub fn tar_bz2(files: &[Entry<'_>]) -> Vec<u8> {
    let mut encoder = bzip2::write::BzEncoder::new(Vec::new(), bzip2::Compression::fast());
    encoder.write_all(&tar(files)).unwrap();
    encoder.finish().unwrap()
}

/// 造一个 `.zip`（不压缩）。
pub fn zip(files: &[Entry<'_>]) -> Vec<u8> {
    let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    for (path, data, mode) in files {
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .unix_permissions(*mode);
        writer.start_file(*path, options).unwrap();
        writer.write_all(data).unwrap();
    }
    writer.finish().unwrap().into_inner()
}

/// 内容的 SHA-256（小写十六进制）。
pub fn sha256(bytes: &[u8]) -> String {
    use sha2::Digest;
    crate::workspace::hex(&sha2::Sha256::digest(bytes))
}
