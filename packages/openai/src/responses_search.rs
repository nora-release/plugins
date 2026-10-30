use serde_json::{json, Map, Value};

pub(super) fn declare(body: &mut Map<String, Value>) {
    let Some(input) = body.get("input").and_then(Value::as_array) else {
        return;
    };
    if !input
        .iter()
        .any(|item| item.get("type").and_then(Value::as_str) == Some("web_search_call"))
    {
        return;
    }
    let mut additional = None;
    let mut offered = false;
    for (index, item) in input.iter().enumerate() {
        if item.get("type").and_then(Value::as_str) != Some("additional_tools") {
            continue;
        }
        let tools = item
            .get("tools")
            .and_then(Value::as_array)
            .map(Vec::as_slice)
            .unwrap_or(&[]);
        if declares_search(tools) {
            return;
        }
        offered |= !tools.is_empty();
        additional.get_or_insert(index);
    }
    let tools = match body.get("tools") {
        None | Some(Value::Null) => &[][..],
        Some(Value::Array(tools)) => tools,
        _ => return,
    };
    if declares_search(tools) {
        return;
    }
    offered |= !tools.is_empty();
    let lite = additional.is_some()
        || body
            .get("reasoning")
            .and_then(|value| value.get("context"))
            .and_then(Value::as_str)
            == Some("all_turns");
    let search = json!({"type":"web_search", "external_web_access":false});
    if let Some(index) = additional {
        let additional = &mut body.get_mut("input").unwrap()[index];
        if !additional["tools"].is_array() {
            additional["tools"] = json!([]);
        }
        additional["tools"].as_array_mut().unwrap().push(search);
    } else if lite {
        body.get_mut("input")
            .and_then(Value::as_array_mut)
            .unwrap()
            .insert(
                0,
                json!({"type":"additional_tools", "role":"developer", "tools":[search]}),
            );
    } else {
        if !body.get("tools").is_some_and(Value::is_array) {
            body.insert("tools".into(), json!([]));
        }
        body.get_mut("tools")
            .and_then(Value::as_array_mut)
            .unwrap()
            .push(search);
    }
    if !offered
        && body.get("tool_choice").is_none_or(|choice| {
            choice.is_null() || matches!(choice.as_str(), Some("" | "auto" | "none"))
        })
    {
        body.insert("tool_choice".into(), json!("none"));
    }
}

fn declares_search(tools: &[Value]) -> bool {
    tools.iter().any(|tool| {
        let kind = tool.get("type").and_then(Value::as_str).unwrap_or("");
        let name = tool.get("name").and_then(Value::as_str).unwrap_or("");
        kind.starts_with("web_search")
            || (kind == "function" && name == "web.run")
            || (kind == "namespace"
                && name == "web"
                && tool
                    .get("tools")
                    .and_then(Value::as_array)
                    .is_some_and(|tools| {
                        tools.iter().any(|tool| {
                            tool.get("type").and_then(Value::as_str) == Some("function")
                                && tool.get("name").and_then(Value::as_str) == Some("run")
                        })
                    }))
    })
}
