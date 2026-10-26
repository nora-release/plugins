#[path = "moonshot_json.rs"]
mod ordered_json;
use ordered_json::{Map, Value};
use std::collections::BTreeSet;

const MAP_KEYS: &[&str] = &[
    "properties",
    "$defs",
    "definitions",
    "dependentSchemas",
    "patternProperties",
];
const SUB_KEYS: &[&str] = &[
    "items",
    "additionalProperties",
    "contains",
    "propertyNames",
    "not",
    "if",
    "then",
    "else",
    "allOf",
    "anyOf",
    "oneOf",
    "prefixItems",
    "unevaluatedProperties",
    "unevaluatedItems",
    "contentSchema",
];
const TYPES: &[&str] = &[
    "string", "number", "integer", "boolean", "object", "array", "null",
];

/// Moonshot rejects otherwise valid JSON Schema shapes emitted by agent tools.
/// Keep accepted request bytes intact so fixing tools does not disturb caches.
pub(super) fn rewrite(bytes: Vec<u8>) -> Vec<u8> {
    if !bytes.windows(7).any(|part| part == b"\"tools\"") {
        return bytes;
    }
    let Ok(mut body) = serde_json::from_slice::<Value>(&bytes) else {
        return bytes;
    };
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return bytes;
    };
    let mut changed = false;
    for tool in tools {
        let Some(parameters) = tool
            .pointer_mut("/function/parameters")
            .filter(|value| value.is_object())
        else {
            continue;
        };
        for stage in 0..3 {
            walk(parameters, "", stage, &mut changed);
        }
    }
    if !changed {
        return bytes;
    }
    serde_json::to_vec(&body).unwrap_or(bytes)
}

fn walk(root: &mut Value, path: &str, stage: u8, changed: &mut bool) {
    let Some(node) = root.pointer(path) else {
        return;
    };
    if let Some(array) = node.as_array() {
        let paths = (0..array.len())
            .map(|index| format!("{path}/{index}"))
            .collect::<Vec<_>>();
        for path in paths {
            walk(root, &path, stage, changed);
        }
        return;
    }
    let Some(object) = node.as_object() else {
        return;
    };
    let replacement = match stage {
        0 => shape(root, object),
        1 if !object.contains_key("type") => enum_type(
            object
                .get("enum")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .or_else(|| object.get("const").map(std::slice::from_ref)),
        )
        .map(|kind| typed(object, kind)),
        2 => {
            let reference = object.get("$ref").and_then(Value::as_str);
            reference
                .and_then(|reference| target(root, reference))
                .and_then(Value::as_object)
                .filter(|target| {
                    !target.is_empty()
                        && !["type", "anyOf", "$ref"]
                            .iter()
                            .any(|key| target.contains_key(*key))
                })
                .and_then(|target| {
                    type_for(root, target)
                        .map(|kind| (reference.unwrap().to_owned(), typed(target, kind)))
                })
                .and_then(|(reference, value)| {
                    let path = reference_path(&reference)?;
                    if let Some(target) = root.pointer_mut(&path) {
                        *target = value;
                        *changed = true;
                    }
                    None
                })
        }
        _ => None,
    };
    if let Some(replacement) = replacement {
        *root.pointer_mut(path).unwrap() = replacement;
        *changed = true;
    }
    let Some(object) = root.pointer(path).and_then(Value::as_object) else {
        return;
    };
    let mut paths = Vec::new();
    for key in MAP_KEYS {
        if let Some(map) = object.get(*key).and_then(Value::as_object) {
            paths.extend(
                map.keys()
                    .map(|name| format!("{path}/{}/{}", escape(key), escape(name))),
            );
        }
    }
    for key in SUB_KEYS {
        if object.contains_key(*key) {
            paths.push(format!("{path}/{}", escape(key)));
        }
    }
    for path in paths {
        walk(root, &path, stage, changed);
    }
}

fn shape(root: &Value, object: &Map) -> Option<Value> {
    let mut output = object.clone();
    let mut changed = false;
    if object.contains_key("$ref") && object.contains_key("type") {
        let target = object["$ref"]
            .as_str()
            .and_then(|reference| target(root, reference))
            .filter(|target| target.is_object() && !has_ref(target));
        if let Some(target) = target {
            let mut merged = target.as_object().unwrap().clone();
            merged.extend(
                object
                    .iter()
                    .filter(|(key, _)| *key != "$ref")
                    .map(|(key, value)| (key.clone(), value.clone())),
            );
            output = merged;
        } else {
            output.remove("type");
        }
        changed = true;
    }
    if output.get("anyOf").is_some_and(Value::is_array) {
        changed |= output.remove("type").is_some();
    }
    changed.then_some(Value::Object(output))
}

fn has_ref(value: &Value) -> bool {
    match value {
        Value::String(value) => value == "$ref",
        Value::Array(values) => values.iter().any(has_ref),
        Value::Object(object) => object
            .iter()
            .any(|(key, value)| key == "$ref" || has_ref(value)),
        _ => false,
    }
}
fn escape(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}
fn reference_tokens(reference: &str) -> Option<Vec<String>> {
    if reference == "#" {
        return Some(Vec::new());
    }
    let reference = reference.strip_prefix("#/")?;
    Some(
        reference
            .split('/')
            .map(|token| {
                let token = token.replace("~1", "/").replace("~0", "~");
                decode(&token).unwrap_or(token)
            })
            .collect(),
    )
}
fn reference_path(reference: &str) -> Option<String> {
    Some(
        reference_tokens(reference)?
            .iter()
            .fold(String::new(), |path, token| {
                format!("{path}/{}", escape(token))
            }),
    )
}
fn target<'a>(root: &'a Value, reference: &str) -> Option<&'a Value> {
    let mut value = root;
    for token in reference_tokens(reference)? {
        value = match value {
            Value::Object(object) => object.get(&token)?,
            Value::Array(array) => {
                let index = token.parse::<usize>().ok()?;
                if index.to_string() != token {
                    return None;
                }
                array.get(index)?
            }
            _ => return None,
        };
    }
    Some(value)
}
fn decode(value: &str) -> Option<String> {
    let mut output = Vec::with_capacity(value.len());
    let bytes = value.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let hex = |value: u8| (value as char).to_digit(16).map(|value| value as u8);
            output.push(hex(*bytes.get(index + 1)?)? * 16 + hex(*bytes.get(index + 2)?)?);
            index += 3;
        } else {
            output.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(output).ok()
}
fn value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::String(_) => "string",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.as_f64().is_some_and(|value| value.fract() == 0.0) => {
            "integer"
        }
        Value::Number(_) => "number",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
fn enum_type(values: Option<&[Value]>) -> Option<Value> {
    let mut kind = None;
    let mut nullable = false;
    for value in values? {
        let next = value_type(value);
        match next {
            "null" => {
                nullable = true;
                continue;
            }
            "array" | "object" => return None,
            _ => {}
        }
        kind = Some(match kind {
            None => next,
            Some(old) if old == next => old,
            Some("integer" | "number") if next == "integer" || next == "number" => "number",
            _ => return None,
        });
    }
    let kind = kind?;
    Some(Value::from(if nullable {
        serde_json::json!([kind, "null"])
    } else {
        serde_json::json!(kind)
    }))
}
fn typed(object: &Map, kind: Value) -> Value {
    let mut output = Map::default();
    output.insert("type".into(), kind);
    output.extend(
        object
            .iter()
            .filter(|(key, _)| *key != "type")
            .map(|(key, value)| (key.clone(), value.clone())),
    );
    Value::Object(output)
}
fn schema_types(root: &Value, object: &Map, depth: u8) -> Option<BTreeSet<String>> {
    if depth > 16 {
        return None;
    }
    if let Some(kind) = object.get("type") {
        let kinds = kind
            .as_array()
            .map(Vec::as_slice)
            .unwrap_or_else(|| std::slice::from_ref(kind));
        return Some(
            kinds
                .iter()
                .map(|kind| {
                    kind.as_str()
                        .map(str::to_owned)
                        .unwrap_or_else(|| kind.to_string())
                })
                .collect(),
        );
    }
    if let Some(reference) = object.get("$ref") {
        return schema_types(
            root,
            target(root, reference.as_str()?)?.as_object()?,
            depth + 1,
        );
    }
    if let Some(values) = object.get("const").map(std::slice::from_ref).or_else(|| {
        object
            .get("enum")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
    }) {
        return Some(
            values
                .iter()
                .map(|value| value_type(value).into())
                .collect(),
        );
    }
    for key in ["oneOf", "anyOf", "allOf"] {
        let Some(branches) = object.get(key).and_then(Value::as_array) else {
            continue;
        };
        let mut types = BTreeSet::new();
        for branch in branches {
            match branch
                .as_object()
                .and_then(|branch| schema_types(root, branch, depth + 1))
            {
                Some(branch) => types.extend(branch),
                None if key != "allOf" => return None,
                None => {}
            }
        }
        if !types.is_empty() {
            return Some(types);
        }
    }
    for (kind, keywords) in [
        (
            "object",
            &[
                "properties",
                "required",
                "additionalProperties",
                "patternProperties",
                "minProperties",
                "maxProperties",
                "propertyNames",
                "dependentRequired",
                "dependentSchemas",
                "unevaluatedProperties",
            ][..],
        ),
        (
            "array",
            &[
                "items",
                "prefixItems",
                "minItems",
                "maxItems",
                "uniqueItems",
                "contains",
                "unevaluatedItems",
            ][..],
        ),
        (
            "string",
            &[
                "minLength",
                "maxLength",
                "pattern",
                "format",
                "contentEncoding",
                "contentMediaType",
            ][..],
        ),
        (
            "number",
            &[
                "minimum",
                "maximum",
                "exclusiveMinimum",
                "exclusiveMaximum",
                "multipleOf",
            ][..],
        ),
    ] {
        if keywords.iter().any(|key| object.contains_key(*key)) {
            return Some(BTreeSet::from([kind.into()]));
        }
    }
    None
}
fn type_for(root: &Value, object: &Map) -> Option<Value> {
    if let Some(values) = object.get("enum").and_then(Value::as_array) {
        return enum_type(Some(values));
    }
    if let Some(value) = object.get("const") {
        return enum_type(Some(std::slice::from_ref(value)));
    }
    let Some(mut types) = schema_types(root, object, 0).filter(|types| !types.is_empty()) else {
        return Some(Value::from(serde_json::json!(TYPES)));
    };
    if types.contains("number") {
        types.remove("integer");
    }
    let names = TYPES
        .iter()
        .filter(|kind| types.contains(**kind))
        .copied()
        .collect::<Vec<_>>();
    Some(Value::from(if names.len() == 1 {
        serde_json::json!(names[0])
    } else {
        serde_json::json!(names)
    }))
}

#[cfg(test)]
#[path = "../test/moonshot.test.rs"]
mod tests;
