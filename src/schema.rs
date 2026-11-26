//! 工具参数格式（JSON Schema）整形成 Gemini 认的写法（设计 73 第四节第 6 条：「Gemini 系照 `antigravity` 包的整形」）：Gemini 系的
//! agent（[`crate::agents::Family::Gemini`]）答 `tools/list` 时每件工具过一遍 [`shape`]；一件写坏了会让整轮的工具全被拒
//! （`properties[site].enum[4]: cannot be empty`），所以宁可多剔。别的 agent 的参数格式原样交。
//!
//! 照 `miyu-antigravity` 的 `schema` 搬（它又是照旧版 Miyu 的 `src/cli/mcp_schema.rs`）。分两层：
//!
//! - [`sanitize`]：`enum` 里的空串、`null`、和声明的 `type` 对不上的项剔掉，剔空了连键一起删；`default` 不在 `enum` 里就剔掉。
//! - [`gemini_compatible`]：只留 Gemini 认的键；`type` 不许是数组；空的 `enum`、空的 `properties`、空的 `required` 不带；
//!   `items` 只属于数组类型；`required` 里引用的属性要真存在。
//!
//! 递归只走 schema 的位置（`properties` 的各个值、`items`，净化那层还有 `anyOf`/`oneOf`/`allOf`）：`properties` 底下名字恰好叫
//! `enum`、`default` 的是**属性名**，不碰。Gemini 那层不认 `anyOf`/`oneOf`/`allOf`，整个键剔掉（旧版如此）。

use serde_json::{Map, Value, json};

/// Gemini 认识的 schema 键；其余一律剔除。
const GEMINI_KEYS: &[&str] = &[
    "type",
    "description",
    "enum",
    "items",
    "properties",
    "required",
    "nullable",
    "format",
    "minimum",
    "maximum",
    "minItems",
    "maxItems",
];

/// 一件工具的参数格式整形成 Gemini 认的写法：先 [`sanitize`]，再 [`gemini_compatible`]。传进来的不是对象的原样交回。
#[must_use]
pub fn shape(schema: Value) -> Value {
    gemini_compatible(sanitize(schema))
}

/// 与供应商无关的净化，只在 schema 位置递归。
#[must_use]
pub fn sanitize(schema: Value) -> Value {
    let Value::Object(mut map) = schema else {
        return schema;
    };
    let declared_type = map.get("type").cloned();
    if let Some(Value::Array(values)) = map.remove("enum") {
        let kept: Vec<Value> = values
            .into_iter()
            .filter(|value| enum_value_fits(value, declared_type.as_ref()))
            .collect();
        if !kept.is_empty() {
            map.insert("enum".into(), Value::Array(kept));
        }
    }
    if let (Some(default), Some(Value::Array(allowed))) = (map.get("default"), map.get("enum"))
        && !allowed.contains(default)
    {
        map.remove("default");
    }
    if let Some(Value::Object(props)) = map.remove("properties") {
        let cleaned: Map<String, Value> = props
            .into_iter()
            .map(|(name, sub)| (name, sanitize(sub)))
            .collect();
        map.insert("properties".into(), Value::Object(cleaned));
    }
    if let Some(items) = map.remove("items") {
        map.insert("items".into(), sanitize(items));
    }
    for key in ["anyOf", "oneOf", "allOf"] {
        if let Some(Value::Array(variants)) = map.remove(key) {
            map.insert(
                key.into(),
                Value::Array(variants.into_iter().map(sanitize).collect()),
            );
        }
    }
    Value::Object(map)
}

/// `enum` 的一项要非空，且和声明的类型对得上（没声明类型就只挡空串和 `null`）。
fn enum_value_fits(value: &Value, declared_type: Option<&Value>) -> bool {
    if value.as_str().is_some_and(str::is_empty) || value.is_null() {
        return false;
    }
    let types: Vec<&str> = match declared_type {
        Some(Value::String(one)) => vec![one.as_str()],
        Some(Value::Array(many)) => many.iter().filter_map(Value::as_str).collect(),
        _ => return true,
    };
    if types.is_empty() {
        return true;
    }
    types.iter().any(|kind| match *kind {
        "string" => value.is_string(),
        "integer" => value.as_i64().is_some() || value.as_u64().is_some(),
        "number" => value.is_number(),
        "boolean" => value.is_boolean(),
        "array" => value.is_array(),
        "object" => value.is_object(),
        "null" => value.is_null(),
        _ => true,
    })
}

/// 只留 Gemini 认的键并改写不认的写法。
#[must_use]
pub fn gemini_compatible(schema: Value) -> Value {
    let Value::Object(map) = schema else {
        return schema;
    };
    let mut out = Map::new();
    let mut nullable = map
        .get("nullable")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    for (key, value) in map {
        if !GEMINI_KEYS.contains(&key.as_str()) {
            continue;
        }
        match key.as_str() {
            "type" => {
                // 联合类型：有 array 就取 array（保住 items，数组形状仍可达），否则取第一个非 null 的；null 折成 nullable。
                let picked = match &value {
                    Value::Array(types) => {
                        if types.iter().any(|kind| kind.as_str() == Some("null")) {
                            nullable = true;
                        }
                        types
                            .iter()
                            .find(|kind| kind.as_str() == Some("array"))
                            .or_else(|| types.iter().find(|kind| kind.as_str() != Some("null")))
                            .cloned()
                            .unwrap_or(Value::String("string".into()))
                    }
                    other => other.clone(),
                };
                out.insert(key, picked);
            }
            "enum" => {
                if let Value::Array(values) = value {
                    let kept: Vec<Value> = values
                        .into_iter()
                        .filter(|v| v.as_str().is_none_or(|text| !text.is_empty()))
                        .collect();
                    if !kept.is_empty() {
                        out.insert(key, Value::Array(kept));
                    }
                }
            }
            "properties" => {
                if let Value::Object(props) = value {
                    let shaped: Map<String, Value> = props
                        .into_iter()
                        .map(|(name, sub)| (name, gemini_compatible(sub)))
                        .collect();
                    // 空 properties 的 OBJECT 会被拒（"should be non-empty"）：干脆不带这个键，让上游按无参处理。
                    if !shaped.is_empty() {
                        out.insert(key, Value::Object(shaped));
                    }
                }
            }
            "items" => {
                out.insert(key, gemini_compatible(value));
            }
            "required" => {
                if let Value::Array(names) = &value
                    && !names.is_empty()
                {
                    out.insert(key, value);
                }
            }
            "nullable" => {}
            _ => {
                out.insert(key, value);
            }
        }
    }
    // items 只属于 array：联合类型折成别的类型后不能留一个孤儿 items。
    if out.get("type").and_then(Value::as_str) != Some("array") {
        out.remove("items");
    }
    // required 里引用的属性要真的存在（属性可能刚被剔掉）。
    if let Some(Value::Array(required)) = out.get("required").cloned() {
        let present = out
            .get("properties")
            .and_then(Value::as_object)
            .map(|props| {
                required
                    .into_iter()
                    .filter(|name| name.as_str().is_some_and(|text| props.contains_key(text)))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if present.is_empty() {
            out.remove("required");
        } else {
            out.insert("required".into(), Value::Array(present));
        }
    }
    if nullable {
        out.insert("nullable".into(), json!(true));
    }
    Value::Object(out)
}

/// agy 自己的工具每件都带一个 `toolAction`（这一步在做什么），模型照这个习惯也往 Miyu 的工具里填；agy 照参数格式校验，
/// 没声明的属性整次调用拒掉（`additional properties 'toolAction' not allowed`，接着整轮中断）。2026-11-23 主人的群聊会话从
/// 某一次起每次工具调用都这样被拒、一直答不出来（施工 AG-11）。这几样声明成可选的文本，转给 Miyu 之前去掉（[`without_extras`]）。
pub const AGY_EXTRAS: &[&str] = &["toolAction"];

/// 给一件工具的参数格式（已经 [`shape`] 过）补上 [`AGY_EXTRAS`]：对象类型（或没写类型）的才补，已经有同名属性的不动。
#[must_use]
pub fn with_extras(schema: Value) -> Value {
    let Value::Object(mut map) = schema else {
        return schema;
    };
    if map.get("type").is_some_and(|kind| kind != "object") {
        return Value::Object(map);
    }
    map.entry("type").or_insert_with(|| json!("object"));
    if let Value::Object(props) = map
        .entry("properties")
        .or_insert_with(|| Value::Object(Map::new()))
    {
        for extra in AGY_EXTRAS {
            props
                .entry(*extra)
                .or_insert_with(|| json!({ "type": "string" }));
        }
    }
    Value::Object(map)
}

/// 转给 Miyu 的参数去掉 [`AGY_EXTRAS`]：那是 agy 的习惯，Miyu 的工具不认。
#[must_use]
pub fn without_extras(arguments: Value) -> Value {
    let Value::Object(mut map) = arguments else {
        return arguments;
    };
    for extra in AGY_EXTRAS {
        map.remove(*extra);
    }
    Value::Object(map)
}

#[cfg(test)]
mod tests;
