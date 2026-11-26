//! `driver.models`（设计 73 第四节第 9 条）：每个这台机器装得上的 agent 一项（模型名就是 agent 的编号，用它自己的默认模型），
//! 设置里手写的 agent 也各一项；用过的 agent 另列它报过的模型（`<agent>/<模型>`）。
//!
//! 列表不为了列模型去起 agent（要装、要登录）：agent 报的模型、看不看图、窗口多大都是用的时候学到的，记在状态目录的
//! `models.json`（[`learn`]）。窗口照最后一次 `usage_update` 的 `size`；还没用过的照惯例写一个保守值（[`default_window`]）。
//!
//! 简称和全名（施工 ACP-10）：Claude 适配器的模型表里既有简称（`haiku`），也有带版本的全名（`claude-haiku-4-5-20251001`）；
//! 简称跟着当代的那一个走（2026-11-26 是 Haiku 5.5，1M 窗口），全名钉在那一版（Haiku 4.5，200k），不是同一个模型。用简称时
//! agent 报了它真用的模型（[`Learned::resolved`]）的，记下「简称 → 全名」，窗口记在全名上；列表里简称照它解到的全名取窗口，
//! 同一个模型的两种叫法窗口一样。

use std::path::Path;
use std::sync::{Mutex, PoisonError};

use serde_json::{Map, Value, json};

use crate::agents;
use crate::catalog::Catalog;
use crate::config::Config;

/// 学到的东西存在哪。
#[must_use]
pub fn file(state: &Path) -> std::path::PathBuf {
    state.join("models.json")
}

/// 写 `models.json` 时一个一个来。
static WRITING: Mutex<()> = Mutex::new(());

/// 一次用下来学到的。
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Learned {
    /// agent 报的模型（`session/new` 的选项）。
    pub models: Option<Vec<String>>,
    /// agent 收不收图（`initialize` 的能力）。
    pub vision: Option<bool>,
    /// 用的模型（`None` 是默认的那个）和它的窗口（`usage_update` 的 `size`）。
    pub window: Option<(Option<String>, u64)>,
    /// 那一次 agent 报的它真用的模型（`usage_update` 的 `_meta["_claude/model"]`）。
    pub resolved: Option<String>,
}

/// 把 agent `agent` 学到的并进 `models.json`。写不了的记一行日志。
pub fn learn(state: &Path, agent: &str, learned: &Learned) {
    let _guard = WRITING.lock().unwrap_or_else(PoisonError::into_inner);
    let path = file(state);
    let mut all: Map<String, Value> = std::fs::read(&path)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    let entry = all.entry(agent.to_string()).or_insert_with(|| json!({}));
    if let Some(models) = &learned.models {
        entry["models"] = json!(models);
    }
    if let Some(vision) = learned.vision {
        entry["vision"] = json!(vision);
    }
    if let Some((model, window)) = &learned.window {
        let key = model.clone().unwrap_or_default();
        for map in ["windows", "resolves"] {
            if !entry[map].is_object() {
                entry[map] = json!({});
            }
        }
        entry["windows"][&key] = json!(window);
        if let Some(resolved) = learned
            .resolved
            .as_ref()
            .filter(|resolved| **resolved != key)
        {
            entry["windows"][resolved] = json!(window);
            entry["resolves"][&key] = json!(resolved);
        }
    }
    let written = std::fs::create_dir_all(state).and_then(|()| {
        let partial = path.with_extension(format!("partial-{}", crate::workspace::unique()));
        std::fs::write(&partial, Value::Object(all).to_string())?;
        std::fs::rename(&partial, &path)
    });
    if let Err(error) = written {
        eprintln!(
            "could not record what {agent} reported in {}: {error}",
            path.display()
        );
    }
}

/// 还没学到窗口时照惯例写的保守值。
#[must_use]
pub fn default_window(agent: &str) -> u64 {
    match agent {
        "claude-acp" => 200_000,
        _ => 128_000,
    }
}

/// 回答 `driver.models` 的 `result`：`ids` 是要列的 agent（登记表里装得上的、手写的），`learned` 是 `models.json` 的内容。
#[must_use]
pub fn answer(ids: &[String], learned: &Map<String, Value>) -> Value {
    let mut models = Vec::new();
    for id in ids {
        let known = learned.get(id);
        let vision = known
            .and_then(|known| known.get("vision"))
            .and_then(Value::as_bool);
        let lookup = |map: &str, model: &str| {
            known.and_then(|known| {
                known.pointer(&format!(
                    "/{map}/{}",
                    model.replace('~', "~0").replace('/', "~1")
                ))
            })
        };
        let window = |model: &str| {
            let resolved = lookup("resolves", model)
                .and_then(Value::as_str)
                .unwrap_or(model);
            lookup("windows", resolved)
                .or_else(|| lookup("windows", model))
                .and_then(Value::as_u64)
                .unwrap_or_else(|| default_window(id))
        };
        let item = |name: String, model: &str| {
            let mut item = json!({"id": name, "window": window(model), "tools": true});
            if let Some(vision) = vision {
                item["vision"] = json!(vision);
            }
            item
        };
        models.push(item(id.clone(), ""));
        for model in known
            .and_then(|known| known.get("models"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
        {
            models.push(item(format!("{id}/{model}"), model));
        }
    }
    json!({"models": models})
}

/// 跑 `driver.models`：登记表照缓存取（过期了重取），手写的 agent 排在前面。
///
/// # Errors
///
/// 登记表取不到、又没有手写的 agent：英文的一句原因。
pub fn list(
    config: &Config,
    state: &Path,
    catalog: impl FnOnce() -> Result<Catalog, String>,
) -> Result<Value, String> {
    let mut ids: Vec<String> = config
        .agents
        .iter()
        .map(|agent| agent.name.clone())
        .collect();
    match catalog() {
        Ok(catalog) => {
            for id in agents::installable(&catalog) {
                if !ids.contains(&id) {
                    ids.push(id);
                }
            }
        }
        Err(why) if ids.is_empty() => return Err(why),
        Err(why) => eprintln!("listing models: {why}; listing only the agents from acp.agents"),
    }
    let learned: Map<String, Value> = std::fs::read(file(state))
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or_default();
    Ok(answer(&ids, &learned))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::scratch;

    #[test]
    fn learned_models_windows_and_vision_show_up() {
        let state = scratch("models");
        learn(
            &state,
            "claude-acp",
            &Learned {
                models: Some(vec!["opus".into(), "sonnet".into()]),
                vision: Some(true),
                window: Some((None, 1_000_000)),
                resolved: None,
            },
        );
        learn(
            &state,
            "claude-acp",
            &Learned {
                window: Some((Some("sonnet".into()), 200_000)),
                ..Learned::default()
            },
        );
        let config = Config::default();
        let listed = list(&config, &state, || {
            crate::catalog::parse(br#"{"agents": [
                {"id": "claude-acp", "distribution": {"npx": {"package": "c@1"}}},
                {"id": "gone", "distribution": {"binary": {"plan9-mips": {"archive": "x", "cmd": "x", "sha256": "x"}}}},
                {"id": "fresh", "distribution": {"uvx": {"package": "f"}}}
            ]}"#)
        })
        .unwrap();
        assert_eq!(
            listed["models"],
            json!([
                {"id": "claude-acp", "window": 1_000_000, "tools": true, "vision": true},
                {"id": "claude-acp/opus", "window": 200_000, "tools": true, "vision": true},
                {"id": "claude-acp/sonnet", "window": 200_000, "tools": true, "vision": true},
                {"id": "fresh", "window": 128_000, "tools": true}
            ])
        );
        std::fs::remove_dir_all(&state).unwrap();
    }

    /// 施工 ACP-10：简称和全名是同一个模型的（agent 报了简称解到哪个全名），两行窗口一样，后学到的两行一起变；不是同一个的
    /// 各是各的（`haiku` 是 Haiku 5.5，`claude-haiku-4-5-20251001` 是 Haiku 4.5）。
    #[test]
    fn an_alias_and_the_full_name_it_runs_share_one_window() {
        let state = scratch("models-alias");
        let all = ["haiku", "claude-haiku-5-5", "claude-haiku-4-5-20251001"];
        learn(
            &state,
            "claude-acp",
            &Learned {
                models: Some(all.iter().map(ToString::to_string).collect()),
                window: Some((Some("haiku".into()), 1_000_000)),
                resolved: Some("claude-haiku-5-5".into()),
                ..Learned::default()
            },
        );
        let windows = |state: &Path| {
            let listed = list(&Config::default(), state, || {
                crate::catalog::parse(br#"{"agents": [{"id": "claude-acp", "distribution": {"npx": {"package": "c@1"}}}]}"#)
            })
            .unwrap();
            listed["models"]
                .as_array()
                .unwrap()
                .iter()
                .map(|item| {
                    (
                        item["id"].as_str().unwrap().to_string(),
                        item["window"].as_u64().unwrap(),
                    )
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            windows(&state)[1..],
            [
                ("claude-acp/haiku".to_string(), 1_000_000),
                ("claude-acp/claude-haiku-5-5".to_string(), 1_000_000),
                ("claude-acp/claude-haiku-4-5-20251001".to_string(), 200_000),
            ]
        );
        learn(
            &state,
            "claude-acp",
            &Learned {
                window: Some((Some("claude-haiku-5-5".into()), 900_000)),
                resolved: Some("claude-haiku-5-5".into()),
                ..Learned::default()
            },
        );
        assert_eq!(windows(&state)[1].1, 900_000, "用全名学到的，简称跟着变");
        std::fs::remove_dir_all(&state).unwrap();
    }

    #[test]
    fn custom_agents_are_listed_even_when_the_registry_is_unreachable() {
        let state = scratch("models-off");
        let mut config = Config::default();
        config.agents.push(crate::config::CustomAgent {
            name: "mine".into(),
            command: "/bin/true".into(),
            args: vec![],
            env: vec![],
        });
        let listed = list(&config, &state, || Err("offline".into())).unwrap();
        assert_eq!(listed["models"][0]["id"], "mine");
        assert!(list(&Config::default(), &state, || Err("offline".into())).is_err());
        std::fs::remove_dir_all(&state).unwrap();
    }
}
