use super::*;
use serde_json::Map;
use std::collections::BTreeSet;

fn safe(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
}

pub(super) fn fit(body: &mut Value) -> PluginResult<BTreeMap<String, String>> {
    let mut names = BTreeMap::new();
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return Ok(names);
    };
    if tools.len() > 1024 {
        return Err(invalid("WorkBuddy accepts at most 1024 tool definitions."));
    }
    let mut taken = tools
        .iter()
        .filter_map(|tool| tool.pointer("/function/name").and_then(Value::as_str))
        .filter(|name| safe(name))
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    let mut outgoing = BTreeMap::new();
    for tool in tools {
        let Some(function) = tool.get_mut("function").and_then(Value::as_object_mut) else {
            continue;
        };
        if let Some(name) = function
            .get("name")
            .and_then(Value::as_str)
            .filter(|name| !safe(name))
            .map(str::to_owned)
        {
            if name.len() > 1024 {
                return Err(invalid("The WorkBuddy tool name is too long."));
            }
            let base = name
                .chars()
                .map(|character| {
                    if character.is_ascii_alphanumeric() || matches!(character, '_' | '-') {
                        character
                    } else {
                        '_'
                    }
                })
                .collect::<String>();
            let base = if base.is_empty() {
                "tool"
            } else {
                base.as_str()
            };
            let mut candidate = base.to_owned();
            let mut index = 2;
            while taken.contains(&candidate) {
                candidate = format!("{base}_{index}");
                index += 1;
            }
            taken.insert(candidate.clone());
            names.insert(candidate.clone(), name.clone());
            outgoing.insert(name, candidate.clone());
            function.insert("name".into(), json!(candidate));
        }
        if let Some(parameters) = function.get_mut("parameters") {
            if !parameters.is_object() {
                *parameters = json!({"type":"object","properties":{}});
                continue;
            }
            fix_schema(parameters, 0)?;
            let root = parameters.clone();
            object_root(parameters, &root, 0)?;
        }
    }
    for message in body
        .get_mut("messages")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        for call in message
            .get_mut("tool_calls")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            if let Some(function) = call.get_mut("function") {
                if let Some(name) = outgoing.get(text(function, "name")) {
                    function["name"] = json!(name);
                }
            }
        }
        if text(message, "role") == "tool" {
            if let Some(name) = outgoing.get(text(message, "name")) {
                message["name"] = json!(name);
            }
        }
    }
    if let Some(choice) = body
        .get_mut("tool_choice")
        .and_then(|choice| choice.get_mut("function"))
    {
        if let Some(name) = outgoing.get(text(choice, "name")) {
            choice["name"] = json!(name);
        }
    }
    Ok(names)
}

fn fix_schema(schema: &mut Value, depth: usize) -> PluginResult<()> {
    if depth > 64 {
        return Err(invalid("The WorkBuddy tool schema is too deeply nested."));
    }
    let Some(object) = schema.as_object_mut() else {
        return Ok(());
    };
    if let Some(value) = object.get("type").cloned() {
        let values = match &value {
            Value::Array(values) => values.clone(),
            value => vec![value.clone()],
        };
        let mut types = Vec::new();
        for value in values {
            if let Some(kind) = value.as_str().map(str::to_ascii_lowercase).filter(|kind| {
                [
                    "string", "number", "integer", "boolean", "object", "array", "null",
                ]
                .contains(&kind.as_str())
            }) {
                if !types.contains(&kind) {
                    types.push(kind);
                }
            }
        }
        if types.is_empty() {
            object.remove("type");
        } else {
            object.insert(
                "type".into(),
                if types.len() == 1 {
                    json!(types[0])
                } else {
                    json!(types)
                },
            );
        }
    }
    if let Some(required) = object.get_mut("required") {
        if let Some(values) = required.as_array_mut() {
            values.retain(Value::is_string);
        } else {
            object.remove("required");
        }
    }
    fit_enum(object);
    if let Some(pattern) = object.get("pattern").and_then(Value::as_str) {
        let mut result = String::new();
        let mut chars = pattern.chars().peekable();
        while let Some(character) = chars.next() {
            result.push(character);
            if character != '\\' {
                continue;
            }
            if chars.peek() == Some(&'\\') {
                result.push(chars.next().unwrap());
                continue;
            }
            if chars.peek() == Some(&'0') {
                let mut tail = chars.clone();
                tail.next();
                if tail
                    .peek()
                    .is_none_or(|character| !character.is_ascii_digit())
                {
                    chars.next();
                    result.push_str("u0000");
                }
            }
        }
        object.insert("pattern".into(), json!(result));
    }
    if let Some(items) = object.get("items").and_then(Value::as_array).cloned() {
        let items = items
            .into_iter()
            .filter(Value::is_object)
            .collect::<Vec<_>>();
        object.insert(
            "items".into(),
            match items.as_slice() {
                [] => json!({}),
                [one] => one.clone(),
                _ => json!({"anyOf":items}),
            },
        );
        object.remove("additionalItems");
    }
    for key in [
        "items",
        "additionalItems",
        "unevaluatedItems",
        "contains",
        "not",
        "if",
        "then",
        "else",
        "propertyNames",
        "additionalProperties",
        "unevaluatedProperties",
    ] {
        if let Some(value) = object.get_mut(key) {
            boolean_schema(value, key);
            fix_schema(value, depth + 1)?;
        }
    }
    for key in ["anyOf", "oneOf", "allOf", "prefixItems"] {
        for value in object
            .get_mut(key)
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            boolean_schema(value, key);
            fix_schema(value, depth + 1)?;
        }
    }
    for key in [
        "properties",
        "patternProperties",
        "$defs",
        "definitions",
        "dependentSchemas",
    ] {
        if let Some(properties) = object.get_mut(key).and_then(Value::as_object_mut) {
            for value in properties.values_mut() {
                boolean_schema(value, key);
                fix_schema(value, depth + 1)?;
            }
        }
    }
    Ok(())
}

fn boolean_schema(value: &mut Value, key: &str) {
    if let Some(flag) = value.as_bool() {
        if !["additionalProperties", "unevaluatedProperties"].contains(&key) {
            *value = if flag { json!({}) } else { json!({"not":{}}) };
        }
    }
}
fn value_type(value: &Value) -> &'static str {
    match value {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(number) if number.is_i64() || number.is_u64() => "integer",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}
fn fit_enum(object: &mut Map<String, Value>) {
    let Some(values) = object.get("enum").and_then(Value::as_array).cloned() else {
        if object.contains_key("enum") {
            object.remove("enum");
        }
        return;
    };
    if values.is_empty() {
        object.remove("enum");
        return;
    }
    let typed = match object.get("type") {
        Some(Value::String(kind)) => usize::from(kind != "null"),
        Some(Value::Array(types)) => types
            .iter()
            .filter(|kind| kind.as_str() != Some("null"))
            .count(),
        _ => 0,
    };
    if typed == 1 {
        return;
    }
    let mut grouped: BTreeMap<&str, Vec<Value>> = BTreeMap::new();
    for value in values {
        grouped.entry(value_type(&value)).or_default().push(value);
    }
    if grouped.contains_key("number") {
        if let Some(integers) = grouped.remove("integer") {
            grouped.get_mut("number").unwrap().extend(integers);
        }
    }
    let nullable = grouped.remove("null").is_some();
    if grouped.len() <= 1 {
        object.insert(
            "type".into(),
            match grouped.keys().next() {
                None => json!("null"),
                Some(kind) if nullable => json!([kind, "null"]),
                Some(kind) => json!(kind),
            },
        );
        return;
    }
    let mut variants = grouped
        .into_iter()
        .map(|(kind, values)| json!({"type":kind,"enum":values}))
        .collect::<Vec<_>>();
    if nullable {
        variants.push(json!({"type":"null"}));
    }
    object.remove("enum");
    object.remove("type");
    if object.get("anyOf").is_some_and(Value::is_array) {
        let all = object.entry("allOf").or_insert(json!([]));
        if !all.is_array() {
            *all = json!([]);
        }
        all.as_array_mut().unwrap().push(json!({"anyOf":variants}));
    } else {
        object.insert("anyOf".into(), json!(variants));
    }
}

fn object_root(value: &mut Value, root: &Value, depth: usize) -> PluginResult<()> {
    if depth > 8 {
        return Err(invalid(
            "The WorkBuddy tool schema has too many nested root unions.",
        ));
    }
    let Some(object) = value.as_object_mut() else {
        return Ok(());
    };
    if object.get("type").and_then(Value::as_str) == Some("object")
        && !["allOf", "anyOf", "oneOf"]
            .iter()
            .any(|key| object.get(*key).is_some_and(Value::is_array))
    {
        return Ok(());
    }
    let mut properties = object
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut required = object
        .get("required")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
    for branch in object
        .get("allOf")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let branch = resolve(branch, root, depth)?;
        for (name, value) in branch
            .get("properties")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            properties
                .entry(name.clone())
                .or_insert_with(|| value.clone());
        }
        required.extend(
            branch
                .get("required")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
    }
    let mut branches = Vec::new();
    for key in ["anyOf", "oneOf"] {
        for branch in object
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let branch = resolve(branch, root, depth)?;
            if text(&branch, "type") == "object" || branch.get("properties").is_some() {
                branches.push(branch);
            }
        }
    }
    let mut alternatives: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    for branch in &branches {
        for (name, value) in branch
            .get("properties")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
        {
            if properties.contains_key(name) {
                continue;
            }
            let alternatives = alternatives.entry(name.clone()).or_default();
            if !alternatives.contains(value) {
                alternatives.push(value.clone());
            }
        }
    }
    for (name, values) in alternatives {
        properties.insert(
            name,
            if values.len() == 1 {
                values[0].clone()
            } else {
                json!({"anyOf":values})
            },
        );
    }
    if let Some(first) = branches.first() {
        for name in first
            .get("properties")
            .and_then(Value::as_object)
            .into_iter()
            .flatten()
            .map(|(name, _)| name)
        {
            if branches.iter().all(|branch| {
                branch
                    .get("required")
                    .and_then(Value::as_array)
                    .is_some_and(|required| {
                        required.iter().any(|value| value.as_str() == Some(name))
                    })
            }) {
                required.insert(name.clone());
            }
        }
    }
    for key in ["allOf", "anyOf", "oneOf"] {
        object.remove(key);
    }
    required.retain(|name| properties.contains_key(name));
    object.insert("type".into(), json!("object"));
    object.insert("properties".into(), json!(properties));
    if required.is_empty() {
        object.remove("required");
    } else {
        object.insert("required".into(), json!(required));
    }
    Ok(())
}
fn resolve(value: &Value, root: &Value, depth: usize) -> PluginResult<Value> {
    let mut value = value
        .get("$ref")
        .and_then(Value::as_str)
        .and_then(|reference| reference.strip_prefix('#'))
        .and_then(|path| root.pointer(path))
        .unwrap_or(value)
        .clone();
    if ["allOf", "anyOf", "oneOf"]
        .iter()
        .any(|key| value.get(*key).is_some_and(Value::is_array))
    {
        object_root(&mut value, root, depth + 1)?;
    }
    Ok(value)
}

pub(super) fn rename(value: &mut Value, names: &BTreeMap<String, String>) {
    for choice in value
        .get_mut("choices")
        .and_then(Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        for key in ["message", "delta"] {
            for call in choice
                .get_mut(key)
                .and_then(|value| value.get_mut("tool_calls"))
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
            {
                if let Some(function) = call.get_mut("function") {
                    if let Some(original) = names.get(text(function, "name")) {
                        function["name"] = json!(original);
                    }
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "../test/tools.test.rs"]
mod tests;
