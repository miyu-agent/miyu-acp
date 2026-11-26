use std::cell::Cell;

use serde_json::json;

use super::*;
use crate::test_support::scratch;

fn registry(version: &str) -> Vec<u8> {
    json!({"version": "1.0.0", "agents": [
        {"id": "demo", "name": "Demo", "version": version, "repository": "https://example.invalid/demo",
         "distribution": {"npx": {"package": format!("demo@{version}")}}},
        {"id": "broken", "name": "No distribution"},
        {"name": "no id", "distribution": {}}
    ], "extensions": []})
    .to_string()
    .into_bytes()
}

/// 写一份缓存，修改时间是 `age` 以前。
fn cache(dir: &Path, bytes: &[u8], age: Duration) {
    let file = cache_file(dir);
    std::fs::write(&file, bytes).unwrap();
    std::fs::File::options()
        .append(true)
        .open(&file)
        .unwrap()
        .set_modified(SystemTime::now() - age)
        .unwrap();
}

#[test]
fn parse_keeps_agents_with_an_id_and_a_distribution() {
    let catalog = parse(&registry("1.0")).unwrap();
    assert_eq!(catalog.agents.len(), 1);
    let demo = catalog.find("demo").unwrap();
    assert_eq!(demo.name, "Demo");
    assert_eq!(demo.version, "1.0");
    assert_eq!(
        demo.website.as_deref(),
        Some("https://example.invalid/demo")
    );
    assert!(parse(b"{}").is_err());
    assert!(parse(b"not json").is_err());
}

#[test]
fn a_fresh_cache_is_used_without_fetching() {
    let dir = scratch("cat-fresh");
    cache(&dir, &registry("1.0"), Duration::from_hours(23));
    let fetched = Cell::new(false);
    let catalog = load(&dir, "u", SystemTime::now(), |_| {
        fetched.set(true);
        Ok(registry("2.0"))
    })
    .unwrap();
    assert!(!fetched.get());
    assert_eq!(catalog.find("demo").unwrap().version, "1.0");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_day_old_cache_is_fetched_again_and_replaced() {
    let dir = scratch("cat-stale");
    cache(&dir, &registry("1.0"), Duration::from_hours(25));
    let catalog = load(&dir, "u", SystemTime::now(), |url| {
        assert_eq!(url, "u");
        Ok(registry("2.0"))
    })
    .unwrap();
    assert_eq!(catalog.find("demo").unwrap().version, "2.0");
    let again = load(&dir, "u", SystemTime::now(), |_| panic!("刚取过，不该再取")).unwrap();
    assert_eq!(again.find("demo").unwrap().version, "2.0");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn an_old_cache_is_used_when_fetching_fails_or_brings_garbage() {
    let dir = scratch("cat-offline");
    cache(&dir, &registry("1.0"), Duration::from_hours(24 * 30));
    let offline = load(&dir, "u", SystemTime::now(), |_| Err("offline".into())).unwrap();
    assert_eq!(offline.find("demo").unwrap().version, "1.0");
    let garbage = load(&dir, "u", SystemTime::now(), |_| Ok(b"<html>".to_vec())).unwrap();
    assert_eq!(garbage.find("demo").unwrap().version, "1.0");
    assert_eq!(
        std::fs::read(cache_file(&dir)).unwrap(),
        registry("1.0"),
        "读不懂的不写进缓存"
    );
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn without_a_cache_a_failed_fetch_is_an_error() {
    let dir = scratch("cat-none");
    let error = load(&dir, "u", SystemTime::now(), |_| Err("offline".into())).unwrap_err();
    assert!(error.contains("offline"), "{error}");
    std::fs::remove_dir_all(&dir).unwrap();
}
