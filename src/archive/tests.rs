use std::os::unix::fs::PermissionsExt;

use super::*;
use crate::test_support::{scratch, tar_bz2, tar_gz, zip};

#[test]
fn kinds_come_from_the_end_of_the_path() {
    for (url, kind) in [
        ("https://x/a.zip", Kind::Zip),
        ("https://x/a.ZIP", Kind::Zip),
        ("https://x/a-linux.tar.gz", Kind::TarGz),
        ("https://x/a.tgz", Kind::TarGz),
        ("https://x/a.tar.bz2", Kind::TarBz2),
        ("https://x/sigit-linux-amd64", Kind::Raw),
        ("https://x/download/?file=muse-x86-linux", Kind::Raw),
        ("https://x/a.zip?token=1", Kind::Zip),
    ] {
        assert_eq!(Kind::of(url), kind, "{url}");
    }
}

#[test]
fn each_kind_unpacks_with_its_permissions() {
    let files: &[(&str, &[u8], u32)] = &[
        ("bin/agent", b"#!/bin/sh\n", 0o755),
        ("README", b"hi", 0o644),
    ];
    for (kind, bytes) in [
        (Kind::Zip, zip(files)),
        (Kind::TarGz, tar_gz(files)),
        (Kind::TarBz2, tar_bz2(files)),
    ] {
        let dir = scratch("unpack");
        let file = dir.join("download");
        std::fs::write(&file, bytes).unwrap();
        let dest = dir.join("out");
        std::fs::create_dir(&dest).unwrap();
        extract(&file, kind, &dest, "./bin/agent").unwrap();
        let mode = std::fs::metadata(dest.join("bin/agent"))
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o111, 0o111, "{kind:?}");
        assert_eq!(std::fs::read(dest.join("README")).unwrap(), b"hi");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}

#[test]
fn a_raw_download_becomes_the_named_program() {
    let dir = scratch("raw");
    let file = dir.join("download");
    std::fs::write(&file, b"\x7fELF").unwrap();
    let dest = dir.join("out");
    std::fs::create_dir(&dest).unwrap();
    extract(&file, Kind::Raw, &dest, "./muse").unwrap();
    let mode = std::fs::metadata(dest.join("muse"))
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111);
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn paths_that_leave_the_folder_are_refused() {
    let dir = scratch("evil");
    let file = dir.join("download");
    std::fs::write(&file, zip(&[("../escape", b"x", 0o644)])).unwrap();
    let dest = dir.join("out");
    std::fs::create_dir(&dest).unwrap();
    let error = extract(&file, Kind::Zip, &dest, "./x").unwrap_err();
    assert!(error.contains("outside"), "{error}");
    assert!(!dir.join("escape").exists());
    std::fs::remove_dir_all(&dir).unwrap();
}
