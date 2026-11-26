use std::os::unix::fs::PermissionsExt;

use serde_json::json;

use super::*;
use crate::test_support::{scratch, sha256, tar_gz};

#[test]
fn the_platform_names_follow_the_registry() {
    let name = platform();
    assert!(
        [
            "linux-x86_64",
            "linux-aarch64",
            "darwin-aarch64",
            "darwin-x86_64"
        ]
        .contains(&name.as_str()),
        "{name}"
    );
}

#[test]
fn a_checked_binary_comes_first_then_npx_then_uvx() {
    let sha = "a".repeat(64);
    let distribution = json!({
        "binary": {"linux-x86_64": {"archive": "https://x/a.tar.gz", "cmd": "./a", "args": ["acp"], "sha256": sha, "env": {"K": "v"}}},
        "npx": {"package": "a@1", "args": ["--acp"]},
        "uvx": {"package": "a==1"}
    });
    assert_eq!(
        plan(&distribution, "linux-x86_64"),
        Some(Plan::Binary {
            archive: "https://x/a.tar.gz".into(),
            sha256: sha.clone(),
            cmd: "./a".into(),
            args: vec!["acp".into()],
            env: vec![("K".into(), "v".into())],
        })
    );
    assert_eq!(
        plan(&distribution, "darwin-aarch64"),
        Some(Plan::Npx {
            package: "a@1".into(),
            args: vec!["--acp".into()],
            env: vec![],
        }),
        "这个系统没有 binary 的用 npx"
    );
    let uvx_only = json!({"uvx": {"package": "a==1", "env": {"M": "x"}}});
    assert_eq!(
        plan(&uvx_only, "linux-x86_64"),
        Some(Plan::Uvx {
            package: "a==1".into(),
            args: vec![],
            env: vec![("M".into(), "x".into())],
        })
    );
}

#[test]
fn a_binary_without_a_checksum_is_never_planned() {
    let unchecked =
        json!({"binary": {"linux-x86_64": {"archive": "https://x/a.zip", "cmd": "./a"}}});
    assert_eq!(plan(&unchecked, "linux-x86_64"), None);
    let with_npx = json!({
        "binary": {"linux-x86_64": {"archive": "https://x/a.zip", "cmd": "./a", "sha256": "bad"}},
        "npx": {"package": "a@1"}
    });
    assert!(matches!(
        plan(&with_npx, "linux-x86_64"),
        Some(Plan::Npx { .. })
    ));
}

#[test]
fn a_musl_build_stands_in_when_there_is_no_glibc_one() {
    let sha = "b".repeat(64);
    let distribution = json!({"binary": {"linux-x86_64-musl": {"archive": "https://x/m.tar.gz", "cmd": "./m", "sha256": sha}}});
    assert!(
        matches!(plan(&distribution, "linux-x86_64"), Some(Plan::Binary { cmd, .. }) if cmd == "./m")
    );
}

/// 在 `dir` 里放一个压缩包，交它的网址和 SHA-256。
fn archive_at(dir: &Path, name: &str, body: &[u8]) -> (String, String) {
    let bytes = tar_gz(&[("bin/agent", body, 0o755)]);
    let file = dir.join(name);
    std::fs::write(&file, &bytes).unwrap();
    (format!("file://{}", file.display()), sha256(&bytes))
}

fn binary_plan(url: &str, sha: &str) -> Plan {
    Plan::Binary {
        archive: url.into(),
        sha256: sha.into(),
        cmd: "./bin/agent".into(),
        args: vec!["acp".into()],
        env: vec![],
    }
}

#[test]
fn a_binary_is_downloaded_checked_unpacked_and_reused() {
    let dir = scratch("inst-ok");
    let (url, sha) = archive_at(&dir, "a.tar.gz", b"#!/bin/sh\necho v1\n");
    let state = dir.join("state");
    let launch = ensure(
        &binary_plan(&url, &sha),
        "demo",
        "1.0",
        &state,
        &dir.join("cache"),
    )
    .unwrap();
    assert_eq!(launch.program, state.join("agents/demo/1.0/./bin/agent"));
    assert_eq!(launch.args, ["acp"]);
    let mode = std::fs::metadata(&launch.program)
        .unwrap()
        .permissions()
        .mode();
    assert_eq!(mode & 0o111, 0o111);
    std::fs::remove_file(dir.join("a.tar.gz")).unwrap();
    let again = ensure(
        &binary_plan(&url, &sha),
        "demo",
        "1.0",
        &state,
        &dir.join("cache"),
    )
    .unwrap();
    assert_eq!(again, launch, "装好的不再下载");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_wrong_checksum_is_refused_and_leaves_nothing() {
    let dir = scratch("inst-bad");
    let (url, _) = archive_at(&dir, "a.tar.gz", b"#!/bin/sh\n");
    let state = dir.join("state");
    let error = ensure(
        &binary_plan(&url, &"0".repeat(64)),
        "demo",
        "1.0",
        &state,
        &dir,
    )
    .unwrap_err();
    assert!(error.contains("SHA-256"), "{error}");
    let left: Vec<_> = std::fs::read_dir(state.join("agents/demo"))
        .unwrap()
        .collect();
    assert!(left.is_empty(), "{left:?}");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_new_version_replaces_the_old_one() {
    let dir = scratch("inst-ver");
    let state = dir.join("state");
    let (one, one_sha) = archive_at(&dir, "1.tar.gz", b"v1");
    ensure(&binary_plan(&one, &one_sha), "demo", "1.0", &state, &dir).unwrap();
    let (two, two_sha) = archive_at(&dir, "2.tar.gz", b"v2");
    ensure(&binary_plan(&two, &two_sha), "demo", "2.0", &state, &dir).unwrap();
    assert!(!state.join("agents/demo/1.0").exists());
    assert!(state.join("agents/demo/2.0/.installed").is_file());
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn programs_are_found_only_when_executable() {
    let dir = scratch("inst-path");
    std::fs::write(dir.join("npx"), "#!/bin/sh\n").unwrap();
    std::fs::write(dir.join("uvx"), "#!/bin/sh\n").unwrap();
    std::fs::set_permissions(dir.join("npx"), std::fs::Permissions::from_mode(0o755)).unwrap();
    let path = std::env::join_paths([dir.join("missing"), dir.clone()]).unwrap();
    assert_eq!(find_program_in("npx", &path), Some(dir.join("npx")));
    assert_eq!(find_program_in("uvx", &path), None, "没有执行权限的不算");
    std::fs::remove_dir_all(&dir).unwrap();
}
