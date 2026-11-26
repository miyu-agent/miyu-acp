//! 握手、`driver.models`、一问一答：增量（正文、思考）、用量（`context`）、系统提示词照能力交、`sent`。

use serde_json::{Value, json};

use crate::support::{Core, request};

#[test]
fn a_turn_streams_thought_and_text_and_reports_usage_with_context() {
    let mut core = Core::start("echo", "fake", "", &json!({}), &[]);
    let got = core.ask("c1", &request("SYS", "hello", &json!([])));
    assert_eq!(got.starts("reasoning").len(), 1);
    assert_eq!(got.starts("text").len(), 1);
    assert_eq!(got.text(), "thinkingpong 1: hello");
    let result = got.result();
    assert_eq!(
        result["usage"],
        json!({"uncached": 11, "cache_read": 3, "cache_write": 0, "output": 7, "context": 4321})
    );
    assert!(result["sent"].as_str().unwrap().starts_with("sha256:"));
    // 不声明文件、终端；工作目录是状态目录下的 work。
    let init = &core.got("initialize")[0];
    assert_eq!(init["clientCapabilities"]["terminal"], false);
    assert_eq!(init["clientCapabilities"]["fs"]["writeTextFile"], false);
    let new = &core.got("session/new")[0];
    assert_eq!(new["cwd"], core.state.join("work").display().to_string());
    assert_eq!(new["mcpServers"], json!([]), "不带工具的不挂 MCP 服务");
}

#[test]
fn the_system_prompt_goes_by_override_append_meta_or_into_the_first_prompt() {
    for (mode, agent) in [
        ("sys=override", "fake"),
        ("sys=append", "fake"),
        ("sys=antigravity", "antigravity-acp"),
        ("sys=none", "fake"),
    ] {
        let mut core = Core::start("sys", agent, mode, &json!({}), &[]);
        let got = core.call_and_collect(agent, &request("BE NICE", "hi", &json!([])));
        assert!(
            got.result().get("error").is_none(),
            "{mode}: {}",
            got.response
        );
        let new = &core.got("session/new")[0];
        let prompt = &core.got("session/prompt")[0]["prompt"][0]["text"];
        let system_text = |value: &Value| {
            value
                .as_str()
                .is_some_and(|text| text.starts_with("BE NICE"))
        };
        match mode {
            "sys=override" => assert_eq!(new["systemPrompt"]["mode"], "override", "{new}"),
            "sys=append" => assert_eq!(new["systemPrompt"]["mode"], "append", "{new}"),
            "sys=antigravity" => {
                assert!(new.get("systemPrompt").is_none());
                assert!(
                    system_text(&new["_meta"]["antigravity"]["systemPrompt"]["append"]),
                    "{new}"
                );
                assert_eq!(
                    new["_meta"]["antigravity"]["eagerMcpServers"],
                    json!(["miyu"])
                );
            }
            _ => {
                assert!(new.get("systemPrompt").is_none());
                assert!(
                    prompt
                        .as_str()
                        .unwrap()
                        .starts_with("<system-instructions>\nBE NICE"),
                    "{prompt}"
                );
            }
        }
        if mode != "sys=none" {
            assert!(
                !prompt.as_str().unwrap().contains("BE NICE"),
                "{mode}: 系统提示词不进提问"
            );
            let system = new.get("systemPrompt").map_or_else(
                || new["_meta"]["antigravity"]["systemPrompt"]["append"].clone(),
                |system| system["content"][0]["text"].clone(),
            );
            assert!(
                system.as_str().unwrap().contains("Miyu ACP bridge"),
                "接上工作目录的说明"
            );
        }
    }
}

#[test]
fn models_list_the_custom_agent_and_what_it_reported() {
    let mut core = Core::start("models", "fake", "models", &json!({}), &[]);
    let before = core.models();
    assert_eq!(
        before["result"]["models"],
        json!([{"id": "fake", "window": 128_000, "tools": true}])
    );
    let mut req = request("S", "hi", &json!([]));
    core.call_with("c1", "c1", &req, "fake/smart", &json!({}));
    let got = core.collect("c1", "c1");
    assert!(got.result().get("error").is_none(), "{}", got.response);
    let selected = &core.got("session/set_config_option")[0];
    assert_eq!(
        (&selected["configId"], &selected["value"]),
        (&json!("model"), &json!("smart"))
    );
    assert_eq!(
        selected["sessionId"],
        core.got("session/prompt")[0]["sessionId"]
    );
    let after = core.models();
    let ids: Vec<&str> = after["result"]["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|m| m["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["fake", "fake/fast", "fake/smart"]);
    assert_eq!(
        after["result"]["models"][2]["window"], 100_000,
        "窗口照 usage_update 的 size"
    );
    assert_eq!(after["result"]["models"][0]["vision"], true);
    req["messages"][0]["blocks"][0]["text"] = json!("again");
    core.call_with("c2", "c2", &req, "fake/turbo", &json!({}));
    let wrong = core.collect("c2", "c2");
    let message = wrong.result()["error"]["message"].as_str().unwrap();
    assert!(
        message.contains("turbo") && message.contains("smart"),
        "{message}"
    );
}

impl Core {
    /// 用模型 `model` 问一次、收齐。
    pub fn call_and_collect(&mut self, model: &str, request: &Value) -> crate::support::Collected {
        self.call_with("one", "one", request, model, &json!({}));
        self.collect("one", "one")
    }
}

#[test]
fn an_image_goes_along_when_the_agent_takes_images() {
    let mut core = Core::start("image", "fake", "", &json!({}), &[]);
    let blob = format!("sha256:{}", "e".repeat(64));
    let mut req = request("S", "look", &json!([]));
    req["messages"][0]["blocks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type": "image", "blob": blob, "name": "cat.png", "media_type": "image/png", "width": 1, "height": 1}));
    core.call_with(
        "c1",
        "c1",
        &req,
        "fake",
        &json!({"blobs": {blob: {"media_type": "image/png", "data": "aGk="}}}),
    );
    let got = core.collect("c1", "c1");
    assert!(got.text().ends_with("[1 image]"), "{}", got.text());
    let prompt = &core.got("session/prompt")[0]["prompt"];
    assert_eq!(
        prompt[1],
        json!({"type": "image", "mimeType": "image/png", "data": "aGk="})
    );
}
