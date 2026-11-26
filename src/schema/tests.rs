use super::*;

// 前四条照旧版 `mcp_schema.rs` 的测试搬来（旧版的「方言」参数没了：有方言的用 `shape`，没方言的用 `sanitize`）。

#[test]
fn empty_enum_entries_and_unknown_keys_are_dropped() {
    let shaped = shape(json!({
        "type": "object",
        "additionalProperties": false,
        "properties": {
            "site": { "type": "string", "enum": ["a", "", "b"], "default": "a", "pattern": "^a" },
            "seat": { "type": ["string", "array"], "items": { "type": "string" } },
            "flag": { "type": ["null", "boolean"] },
            "orphan": { "type": ["string", "integer"], "items": { "type": "string" } }
        },
        "required": ["site", "gone"]
    }));
    assert!(shaped.get("additionalProperties").is_none());
    assert_eq!(shaped["properties"]["site"]["enum"], json!(["a", "b"]));
    assert!(shaped["properties"]["site"].get("default").is_none());
    assert!(shaped["properties"]["site"].get("pattern").is_none());
    assert_eq!(shaped["properties"]["seat"]["type"], "array");
    assert!(shaped["properties"]["seat"].get("items").is_some());
    assert_eq!(shaped["properties"]["flag"]["type"], "boolean");
    assert_eq!(shaped["properties"]["flag"]["nullable"], true);
    assert_eq!(shaped["required"], json!(["site"]));
    assert_eq!(shaped["properties"]["orphan"]["type"], "string");
    assert!(shaped["properties"]["orphan"].get("items").is_none());
}

#[test]
fn empty_object_properties_are_omitted_and_sanitize_alone_keeps_the_rest() {
    let raw = json!({ "type": "object", "properties": {}, "additionalProperties": false });
    assert_eq!(shape(raw.clone()), json!({ "type": "object" }));
    assert_eq!(sanitize(raw.clone()), raw, "净化那层不碰别的键");
}

/// 净化对 gemini 和不整形都生效：空串、类型不符的 enum 项剔掉，剔空连键删，default 不在 enum 里剔掉；
/// items 与 anyOf/oneOf/allOf 里的也一样。
#[test]
fn sanitizing_drops_bad_enum_items() {
    let raw = json!({
        "type": "object",
        "properties": {
            "site": { "type": "string", "enum": ["zh", "cn", "uk", "ja", ""], "default": "cn" },
            "count": { "type": "integer", "enum": [1, "two", 3, null], "default": "two" },
            "gone": { "type": "string", "enum": ["", ""] },
            "list": { "type": "array", "items": { "type": "string", "enum": ["a", ""] } },
            "either": { "anyOf": [ { "type": "string", "enum": ["", "x"] }, { "type": "integer" } ] },
            "loose": { "enum": ["k", "", 2] }
        }
    });
    for shaped in [sanitize(raw.clone()), shape(raw.clone())] {
        let props = &shaped["properties"];
        assert_eq!(props["site"]["enum"], json!(["zh", "cn", "uk", "ja"]));
        assert_eq!(props["count"]["enum"], json!([1, 3]));
        assert!(
            props["count"].get("default").is_none(),
            "default 不在 enum 里要剔掉"
        );
        assert!(props["gone"].get("enum").is_none(), "剔空了连键一起删");
        assert_eq!(props["list"]["items"]["enum"], json!(["a"]));
        assert_eq!(
            props["loose"]["enum"],
            json!(["k", 2]),
            "没声明类型只挡空串"
        );
    }
    // 只净化：default 还在、anyOf 还在；Gemini 那层不认 anyOf，整个键被剔。
    let plain = sanitize(raw.clone());
    assert_eq!(plain["properties"]["site"]["default"], "cn");
    assert_eq!(
        plain["properties"]["either"]["anyOf"][0]["enum"],
        json!(["x"])
    );
    assert!(shape(raw)["properties"]["either"].get("anyOf").is_none());
}

/// 递归只走 schema 位置：`properties` 底下名字就叫 enum / default / items 的**属性**原样保留。
#[test]
fn properties_named_like_keywords_survive() {
    let raw = json!({
        "type": "object",
        "properties": {
            "enum": { "type": "string", "description": "a field that happens to be called enum" },
            "default": { "type": "integer" },
            "items": { "type": "array", "items": { "type": "string", "enum": ["", "q"] } },
            "nested": {
                "type": "object",
                "properties": { "enum": { "type": "string", "enum": ["", "z"] } }
            }
        }
    });
    for shaped in [sanitize(raw.clone()), shape(raw)] {
        let props = &shaped["properties"];
        assert_eq!(props["enum"]["type"], "string");
        assert_eq!(props["default"]["type"], "integer");
        assert_eq!(props["items"]["items"]["enum"], json!(["q"]));
        assert_eq!(props["nested"]["properties"]["enum"]["enum"], json!(["z"]));
    }
}

#[test]
fn nested_objects_are_shaped_all_the_way_down() {
    let shaped = shape(json!({
        "type": "object",
        "properties": {
            "outer": {
                "type": "object",
                "additionalProperties": true,
                "properties": {
                    "inner": {
                        "type": ["null", "string"],
                        "enum": ["", "v"],
                        "default": "v",
                        "pattern": "^v"
                    },
                    "empty": { "type": "object", "properties": {} }
                },
                "required": ["inner", "missing"]
            }
        }
    }));
    assert_eq!(
        shaped,
        json!({
            "type": "object",
            "properties": {
                "outer": {
                    "type": "object",
                    "properties": {
                        "inner": { "type": "string", "enum": ["v"], "nullable": true },
                        "empty": { "type": "object" }
                    },
                    "required": ["inner"]
                }
            }
        })
    );
}

#[test]
fn array_items_are_shaped_and_a_non_array_type_loses_items() {
    let shaped = shape(json!({
        "type": "array",
        "items": {
            "type": "object",
            "additionalProperties": false,
            "properties": { "kind": { "type": "string", "enum": ["", "a"] } },
            "required": ["kind", "other"]
        },
        "minItems": 1,
        "uniqueItems": true
    }));
    assert_eq!(
        shaped,
        json!({
            "type": "array",
            "items": {
                "type": "object",
                "properties": { "kind": { "type": "string", "enum": ["a"] } },
                "required": ["kind"]
            },
            "minItems": 1
        })
    );
    assert_eq!(
        shape(json!({"type": "string", "items": {"type": "string"}})),
        json!({"type": "string"})
    );
}

#[test]
fn any_of_members_are_sanitized_before_gemini_drops_the_keyword() {
    let raw = json!({
        "anyOf": [
            { "type": "array", "items": { "type": "string", "enum": ["", "k"] } },
            { "type": "string", "enum": ["", ""] }
        ],
        "oneOf": [ { "type": "integer", "enum": [1, "x"] } ],
        "allOf": [ { "type": "string", "enum": ["", "z"], "default": "" } ]
    });
    let plain = sanitize(raw.clone());
    assert_eq!(plain["anyOf"][0]["items"]["enum"], json!(["k"]));
    assert!(plain["anyOf"][1].get("enum").is_none());
    assert_eq!(plain["oneOf"][0]["enum"], json!([1]));
    assert_eq!(plain["allOf"][0]["enum"], json!(["z"]));
    assert!(plain["allOf"][0].get("default").is_none());
    assert_eq!(shape(raw), json!({}));
}

#[test]
fn non_objects_pass_through_and_an_empty_required_is_dropped() {
    assert_eq!(shape(json!(true)), json!(true));
    assert_eq!(shape(json!(null)), json!(null));
    let shaped =
        shape(json!({"type": "object", "properties": {"a": {"type": "string"}}, "required": []}));
    assert!(shaped.get("required").is_none());
    assert_eq!(shape(json!({"type": ["null"]}))["nullable"], true);
}

/// 施工 AG-11：agy 照参数格式校验，模型照 agy 自己工具的习惯多填的 `toolAction` 没声明就整次调用被拒。
#[test]
fn agy_extras_are_declared_optional_and_stripped_before_miyu() {
    let shaped = with_extras(shape(json!({
        "type": "object",
        "properties": { "msg": { "type": "string" } },
        "required": ["msg"],
        "additionalProperties": false
    })));
    assert_eq!(
        shaped["properties"]["toolAction"],
        json!({ "type": "string" })
    );
    assert_eq!(shaped["required"], json!(["msg"]), "补的是可选的");
    // 没有参数的工具（整形后只剩 `{"type":"object"}`）也补上。
    let bare = with_extras(shape(json!({ "type": "object", "properties": {} })));
    assert_eq!(
        bare["properties"]["toolAction"],
        json!({ "type": "string" })
    );
    // 已经有同名属性的照工具自己的。
    let own = with_extras(
        json!({ "type": "object", "properties": { "toolAction": { "type": "integer" } } }),
    );
    assert_eq!(own["properties"]["toolAction"]["type"], "integer");
    assert_eq!(
        without_extras(json!({ "msg": "1", "toolAction": "Quoting the message" })),
        json!({ "msg": "1" })
    );
}
