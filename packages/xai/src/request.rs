use serde_json::{json, Map, Value};
use std::collections::BTreeMap;

fn supported_tool(kind: Option<&Value>) -> bool {
    matches!(
        kind.and_then(Value::as_str),
        Some(
            "function"
                | "web_search"
                | "x_search"
                | "image_generation"
                | "collections_search"
                | "file_search"
                | "code_execution"
                | "code_interpreter"
                | "mcp"
                | "shell"
                | "tool_search"
        )
    )
}

pub(super) fn rewrite(payload: &mut Value, x_search: bool) -> bool {
    let Some(root) = payload.as_object_mut() else {
        return false;
    };
    let mut changed = false;
    let automatic = root.get("tool_choice").is_none_or(Value::is_null)
        || root.get("tool_choice").and_then(Value::as_str) == Some("auto");
    let search = x_search
        && automatic
        && root
            .get("tools")
            .is_none_or(|tools| tools.is_null() || tools.is_array())
        && (root.get("tool_choice").and_then(Value::as_str) == Some("auto")
            || root
                .get("tools")
                .and_then(Value::as_array)
                .is_some_and(|tools| !tools.is_empty()));
    if let Some(tools) = root.get_mut("tools").and_then(Value::as_array_mut) {
        tools.retain_mut(|tool| {
            let tool = match tool {
                Value::Object(tool) => tool,
                Value::Array(_) => {
                    changed = true;
                    return false;
                }
                _ => return true,
            };
            if !supported_tool(tool.get("type")) {
                changed = true;
                return false;
            }
            changed |= tool.remove("external_web_access").is_some();
            if tool.get("type").and_then(Value::as_str) != Some("function") {
                return true;
            }
            let function = if tool.get("function").is_some_and(Value::is_object) {
                tool.get_mut("function").unwrap().as_object_mut().unwrap()
            } else {
                tool
            };
            if let Some(parameters) = function.get("parameters").and_then(object_root) {
                function.insert("parameters".into(), parameters);
                changed = true;
            }
            true
        });
        if root
            .get("tool_choice")
            .filter(|choice| choice.is_object() || choice.is_array())
            .is_some_and(|choice| !supported_tool(choice.get("type")))
        {
            root.remove("tool_choice");
            changed = true;
        }
    }
    if search
        && (root.get("tool_choice").and_then(Value::as_str) == Some("auto")
            || root
                .get("tools")
                .and_then(Value::as_array)
                .is_some_and(|tools| !tools.is_empty()))
        && !root
            .get("tools")
            .and_then(Value::as_array)
            .is_some_and(|tools| {
                tools
                    .iter()
                    .any(|tool| tool.get("type").and_then(Value::as_str) == Some("x_search"))
            })
    {
        let tools = root.entry("tools").or_insert_with(|| json!([]));
        if tools.is_null() {
            *tools = json!([]);
        }
        tools
            .as_array_mut()
            .unwrap()
            .push(json!({"type":"x_search"}));
        changed = true;
    }
    if !root
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty())
    {
        changed |= root.remove("tool_choice").is_some();
    }
    if let Some(input) = root.get_mut("input").and_then(Value::as_array_mut) {
        for item in input {
            if let Some(message) = agent_message(item) {
                *item = message;
                changed = true;
            }
            let Some(item) = item.as_object_mut() else {
                continue;
            };
            if item.get("type").and_then(Value::as_str) == Some("reasoning")
                && item.get("content") == Some(&Value::Null)
            {
                item.remove("content");
                changed = true;
            }
        }
    }
    changed
}

fn agent_message(item: &Value) -> Option<Value> {
    let item = item.as_object()?;
    if item.get("type").and_then(Value::as_str) != Some("agent_message")
        || item.contains_key("encrypted_content")
    {
        return None;
    }
    let content = item.get("content")?.as_array()?;
    if content.is_empty()
        || !content.iter().all(|part| {
            let Some(part) = part.as_object() else {
                return false;
            };
            !part.contains_key("encrypted_content")
                && match part.get("type").and_then(Value::as_str) {
                    Some("input_text") => part.get("text").is_some_and(Value::is_string),
                    Some("input_image") => part.get("image_url").is_some_and(Value::is_string),
                    _ => false,
                }
        })
    {
        return None;
    }
    let from = item.get("author").and_then(Value::as_str).unwrap_or("");
    let to = item.get("recipient").and_then(Value::as_str).unwrap_or("");
    let header = match (from.is_empty(), to.is_empty()) {
        (false, false) => format!("From {from} to {to}\n\n"),
        (false, true) => format!("From {from}\n\n"),
        (true, false) => format!("To {to}\n\n"),
        (true, true) => String::new(),
    };
    let mut parts = Vec::with_capacity(content.len() + usize::from(!header.is_empty()));
    if !header.is_empty() {
        parts.push(json!({"type":"input_text","text":header}));
    }
    parts.extend(content.iter().cloned());
    Some(json!({"type":"message","role":"user","content":parts}))
}

fn object_root(parameters: &Value) -> Option<Value> {
    let schema = parameters.as_object()?;
    if plain_object(schema) {
        return None;
    }
    Some(fold_root(parameters, schema, 0))
}

fn plain_object(schema: &Map<String, Value>) -> bool {
    schema.get("type").and_then(Value::as_str) == Some("object")
        && !["anyOf", "oneOf", "allOf"]
            .iter()
            .any(|key| schema.contains_key(*key))
}

fn branch<'a>(root: &'a Value, value: &'a Value) -> Option<&'a Map<String, Value>> {
    let object = value.as_object()?;
    let Some(reference) = object
        .get("$ref")
        .and_then(Value::as_str)
        .filter(|reference| !reference.is_empty())
    else {
        return Some(object);
    };
    for definitions in ["$defs", "definitions"] {
        if let Some(key) = reference.strip_prefix(&format!("#/{definitions}/")) {
            return root.get(definitions)?.get(key)?.as_object();
        }
    }
    None
}

fn fold_root(root: &Value, schema: &Map<String, Value>, depth: usize) -> Value {
    if plain_object(schema) {
        return Value::Object(schema.clone());
    }
    let mut output = schema.clone();
    let mut properties = schema
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut required = schema
        .get("required")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for entry in schema
        .get("allOf")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let Some(branch) = branch(root, entry) else {
            continue;
        };
        if let Some(fields) = branch.get("properties").and_then(Value::as_object) {
            for (key, value) in fields {
                properties
                    .entry(key.clone())
                    .or_insert_with(|| value.clone());
            }
        }
        if let Some(fields) = branch.get("required").and_then(Value::as_array) {
            required.extend(fields.iter().cloned());
        }
    }
    output.remove("allOf");
    let mut variants: BTreeMap<String, Vec<Value>> = BTreeMap::new();
    let mut common: Option<Vec<Value>> = None;
    for union in ["anyOf", "oneOf"] {
        for entry in schema
            .get(union)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(branch) = branch(root, entry) else {
                continue;
            };
            let folded;
            let branch = if depth < 8
                && ["anyOf", "oneOf", "allOf"]
                    .iter()
                    .any(|key| branch.contains_key(*key))
            {
                folded = fold_root(root, branch, depth + 1);
                folded.as_object().unwrap()
            } else {
                branch
            };
            if branch.get("type").and_then(Value::as_str) != Some("object")
                && !branch.contains_key("properties")
            {
                continue;
            }
            if let Some(fields) = branch.get("properties").and_then(Value::as_object) {
                for (key, value) in fields {
                    if properties.contains_key(key) {
                        continue;
                    }
                    let values = variants.entry(key.clone()).or_default();
                    if !values.contains(value) {
                        values.push(value.clone());
                    }
                }
            }
            let fields = branch
                .get("required")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();
            match &mut common {
                Some(common) => common.retain(|field| fields.contains(field)),
                None => common = Some(fields),
            }
        }
        output.remove(union);
    }
    for (key, mut values) in variants {
        properties.insert(
            key,
            if values.len() == 1 {
                values.remove(0)
            } else {
                json!({"anyOf":values})
            },
        );
    }
    let mut unique = Vec::new();
    required.extend(common.into_iter().flatten());
    for field in required {
        if !unique.contains(&field) {
            unique.push(field);
        }
    }
    output.insert("type".into(), json!("object"));
    output.insert("properties".into(), Value::Object(properties));
    if unique.is_empty() {
        output.remove("required");
    } else {
        output.insert("required".into(), json!(unique));
    }
    Value::Object(output)
}
