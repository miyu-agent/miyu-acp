//! 照登记表装：`binary` 分发下载、核 SHA-256、解开、起；核不上的拒装；`driver.models` 列登记表里装得上的。登记表照环境变量
//! `MIYU_ACP_REGISTRY_URL` 指到本机文件，压缩包也是 `file://`，不上网。

use std::io::Write;

use serde_json::json;
use sha2::{Digest, Sha256};

use crate::support::{Core, fake, hex, request};

/// 一个 `.tar.gz`：里面的 `bin/demo` 起假 agent。交字节。
fn archive() -> Vec<u8> {
    let script = format!("#!/bin/sh\nexec python3 '{}' \"$@\"\n", fake().display());
    let mut builder = tar::Builder::new(Vec::new());
    let mut header = tar::Header::new_gnu();
    header.set_size(script.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    builder
        .append_data(&mut header, "bin/demo", script.as_bytes())
        .unwrap();
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
    encoder.write_all(&builder.into_inner().unwrap()).unwrap();
    encoder.finish().unwrap()
}

/// 这台机器在登记表里的写法。
fn platform() -> String {
    let os = if cfg!(target_os = "macos") {
        "darwin"
    } else {
        "linux"
    };
    format!("{os}-{}", std::env::consts::ARCH)
}

/// 在 `core` 的根目录里放压缩包和登记表；`sha` 是登记表里写的校验和（`None` 的照实算）。
fn publish(core: &Core, sha: Option<&str>) {
    let bytes = archive();
    let file = core.root().join("demo.tar.gz");
    std::fs::write(&file, &bytes).unwrap();
    let real = hex(&Sha256::digest(&bytes));
    let registry = json!({"version": "1.0.0", "agents": [{
        "id": "demo", "name": "Demo Agent", "version": "1.0", "website": "https://example.invalid/demo",
        "distribution": {"binary": {platform(): {
            "archive": format!("file://{}", file.display()),
            "cmd": "./bin/demo",
            "sha256": sha.unwrap_or(&real),
            "env": {"FAKE_MODE": "sys=append"}
        }}}
    }, {"id": "elsewhere", "distribution": {"binary": {"plan9-mips": {"archive": "x", "cmd": "x", "sha256": "0".repeat(64)}}}}]});
    std::fs::write(core.root().join("registry.json"), registry.to_string()).unwrap();
}

#[test]
fn a_registry_agent_is_downloaded_checked_and_run() {
    let mut core = Core::start("install", "fake", "", &json!({}), &[]);
    publish(&core, None);
    let ids: Vec<String> = core.models()["result"]["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| model["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(ids, ["fake", "demo"], "装不上的（elsewhere）不列");
    let got = core.call_and_collect("demo", &request("S", "hello", &json!([])));
    assert_eq!(got.text(), "thinkingpong 1: hello", "{}", got.response);
    assert!(core.state.join("agents/demo/1.0/.installed").is_file());
    assert_eq!(
        core.got("session/new")[0]["systemPrompt"]["mode"],
        "append",
        "登记表的 env 交给了 agent"
    );
    assert!(core.cache.join("registry.json").is_file(), "登记表缓存了");
}

#[test]
fn a_download_with_the_wrong_checksum_is_refused() {
    let mut core = Core::start("badsum", "fake", "", &json!({}), &[]);
    publish(&core, Some(&"0".repeat(64)));
    let got = core.call_and_collect("demo", &request("S", "hello", &json!([])));
    let message = got.result()["error"]["message"].as_str().unwrap();
    assert!(message.contains("SHA-256"), "{message}");
    assert!(!core.state.join("agents/demo/1.0").exists());
    assert!(core.got("initialize").is_empty(), "没起");
}

#[test]
fn an_unknown_agent_is_a_clear_error() {
    let mut core = Core::start("unknown", "fake", "", &json!({}), &[]);
    publish(&core, None);
    let got = core.call_and_collect("nobody", &request("S", "hello", &json!([])));
    let message = got.result()["error"]["message"].as_str().unwrap();
    assert!(message.contains("nobody"), "{message}");
}
