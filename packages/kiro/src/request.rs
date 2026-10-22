use super::session;
use base64::Engine;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
pub(crate) type Names = BTreeMap<String, String>;
pub(crate) fn id(value: &str) -> String {
    if !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_.:-".contains(&b))
    {
        return value.into();
    }
    format!(
        "t_{}",
        &base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(value.as_bytes()))
            [..32]
    )
}
pub(crate) fn name(value: &str) -> String {
    if !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b))
    {
        return value.into();
    }
    let head = value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(51)
        .collect::<String>();
    format!("{head}_{}", &session::fingerprint(value)[..12])
}
pub(crate) fn thinking(req: &Value, model: &str, effort: Option<&str>) -> u64 {
    if model != "auto" && !model.to_ascii_lowercase().contains("claude") {
        return 0;
    }
    if req.pointer("/thinking/type").and_then(Value::as_str) == Some("disabled")
        || effort == Some("none")
    {
        return 0;
    }
    let effort = effort
        .or_else(|| req.pointer("/output_config/effort").and_then(Value::as_str))
        .unwrap_or_default();
    let on = ["enabled", "adaptive"].contains(
        &req.pointer("/thinking/type")
            .and_then(Value::as_str)
            .unwrap_or_default(),
    ) || !effort.is_empty() && effort != "low";
    if !on {
        return 0;
    }
    match effort {
        "low" => 10000,
        "high" => 30000,
        "xhigh" | "max" | "ultra" => 50000,
        "" => req
            .pointer("/thinking/budget_tokens")
            .and_then(Value::as_u64)
            .filter(|budget| *budget > 0)
            .unwrap_or(20000)
            .min(50000),
        _ => 20000,
    }
}
fn text(value: &Value) -> String {
    if let Some(text) = value.as_str() {
        return text.into();
    }
    value
        .as_array()
        .into_iter()
        .flatten()
        .filter(|part| part["type"] == "text")
        .filter_map(|part| part["text"].as_str())
        .collect::<Vec<_>>()
        .join("\n\n")
}
fn append(target: &mut Value, value: &str) {
    let existing = target.as_str().unwrap_or_default();
    *target = json!(if existing.is_empty() {
        value.into()
    } else if value.is_empty() {
        existing.into()
    } else {
        format!("{existing}\n\n{value}")
    });
}
fn image(part: &Value) -> Option<Value> {
    if part.pointer("/source/type").and_then(Value::as_str) != Some("base64") {
        return None;
    }
    let data = part
        .pointer("/source/data")
        .and_then(Value::as_str)
        .filter(|data| !data.is_empty())?;
    let media = part
        .pointer("/source/media_type")
        .and_then(Value::as_str)
        .unwrap_or("image/png");
    let format = media.strip_prefix("image/").unwrap_or("png");
    Some(json!({"format":if format=="jpg"{"jpeg"}else{format},"source":{"bytes":data}}))
}
fn user(model: &str) -> Value {
    json!({"content":"","modelId":model,"origin":"KIRO_CLI","images":[]})
}
fn parts(value: &Value) -> Vec<Value> {
    if let Some(text) = value.as_str() {
        return vec![json!({"type":"text","text":text})];
    }
    value.as_array().cloned().unwrap_or_default()
}
pub(crate) fn uuid() -> String {
    let raw = nora_ai::oauth_pkce::random_hex(16);
    format!(
        "{}-{}-{}-{}-{}",
        &raw[..8],
        &raw[8..12],
        &raw[12..16],
        &raw[16..20],
        &raw[20..]
    )
}
pub(crate) fn build(
    req: &Value,
    model: &str,
    profile: &str,
    budget: u64,
) -> PluginResult<(Value, Names)> {
    let messages = req["messages"]
        .as_array()
        .ok_or_else(|| PluginError::invalid("Kiro requires Anthropic messages."))?;
    if messages.len() > 10000 {
        return Err(PluginError::invalid(
            "The Kiro conversation has too many messages.",
        ));
    }
    let mut entries: Vec<Value> = Vec::new();
    let mut names = Names::new();
    for message in messages {
        let blocks = parts(&message["content"]);
        let marked = blocks
            .iter()
            .any(|part| part.get("cache_control").is_some());
        let assistant = message["role"] == "assistant";
        let key = if assistant {
            "assistantResponseMessage"
        } else {
            "userInputMessage"
        };
        let mut output = if assistant {
            json!({"content":"","toolUses":[]})
        } else {
            user(model)
        };
        let mut results = Vec::new();
        for part in blocks {
            match part["type"].as_str().unwrap_or_default() {
                "text" => append(
                    &mut output["content"],
                    part["text"].as_str().unwrap_or_default(),
                ),
                "tool_use" if assistant => {
                    let original = part["name"].as_str().unwrap_or_default();
                    let wire = name(original);
                    if wire != original {
                        names.insert(wire.clone(), original.into());
                    }
                    output["toolUses"].as_array_mut().unwrap().push(json!({"name":wire,"toolUseId":id(part["id"].as_str().unwrap_or_default()),"input":part.get("input").filter(|value|value.is_object()).cloned().unwrap_or_else(||json!({}))}));
                }
                "tool_result" if !assistant => {
                    let content = text(&part["content"]);
                    let truncated = content.chars().take(250000).collect::<String>();
                    let content = if content.chars().count() > 250000 {
                        format!("{truncated}\n… (cut)")
                    } else {
                        content
                    };
                    results.push(json!({"toolUseId":id(part["tool_use_id"].as_str().unwrap_or_default()),"status":if part["is_error"]==true{"error"}else{"success"},"content":[{"text":if content.is_empty(){"(no output)"}else{&content}}]}));
                    for part in parts(&part["content"]) {
                        if let Some(image) = image(&part) {
                            output["images"].as_array_mut().unwrap().push(image);
                        }
                    }
                }
                "image" if !assistant => {
                    if let Some(image) = image(&part) {
                        output["images"].as_array_mut().unwrap().push(image);
                    }
                }
                "document" if !assistant => {
                    let body = match part.pointer("/source/type").and_then(Value::as_str) {
                        Some("text") => part
                            .pointer("/source/data")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .into(),
                        Some("content") => text(&part["source"]["content"]),
                        _ => format!(
                            "[a {} attachment]",
                            part.pointer("/source/media_type")
                                .and_then(Value::as_str)
                                .unwrap_or("document")
                        ),
                    };
                    append(
                        &mut output["content"],
                        &format!(
                            "{}{}",
                            part["title"]
                                .as_str()
                                .map(|title| format!("{title}\n"))
                                .unwrap_or_default(),
                            body
                        ),
                    );
                }
                _ => {}
            }
        }
        if assistant && output["content"] == "" && output["toolUses"].as_array().unwrap().is_empty()
        {
            continue;
        }
        if !results.is_empty() {
            output["userInputMessageContext"] = json!({"toolResults":results});
        }
        if marked {
            output["cachePoint"] = json!({"type":"default"});
        }
        if let Some(previous) = entries.last_mut().and_then(|entry| entry.get_mut(key)) {
            append(
                &mut previous["content"],
                output["content"].as_str().unwrap_or_default(),
            );
            for list in if assistant {
                vec!["toolUses"]
            } else {
                vec!["images"]
            } {
                previous[list]
                    .as_array_mut()
                    .unwrap()
                    .extend(output[list].as_array().cloned().unwrap_or_default());
            }
            if !assistant {
                let added = output
                    .pointer("/userInputMessageContext/toolResults")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();
                if !added.is_empty() {
                    if previous.get("userInputMessageContext").is_none() {
                        previous["userInputMessageContext"] = json!({"toolResults":[]});
                    }
                    previous["userInputMessageContext"]["toolResults"]
                        .as_array_mut()
                        .unwrap()
                        .extend(added);
                }
            }
            if marked {
                previous["cachePoint"] = json!({"type":"default"});
            }
        } else {
            entries.push(json!({key:output}));
        }
    }
    while let Some(first) = entries.first_mut() {
        if let Some(user) = first.get_mut("userInputMessage") {
            if user["content"] == ""
                && user["images"].as_array().is_some_and(Vec::is_empty)
                && user.get("userInputMessageContext").is_some()
            {
                entries.remove(0);
                continue;
            }
            user.as_object_mut()
                .unwrap()
                .remove("userInputMessageContext");
            break;
        }
        entries.remove(0);
    }
    if entries
        .last()
        .and_then(|entry| entry.get("userInputMessage"))
        .is_none()
    {
        entries.push(json!({"userInputMessage":user(model)}));
    }
    for index in 0..entries.len() {
        if entries[index].get("userInputMessage").is_none() {
            continue;
        }
        let calls = index
            .checked_sub(1)
            .and_then(|previous| entries[previous].pointer("/assistantResponseMessage/toolUses"))
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let user = &mut entries[index]["userInputMessage"];
        let results = user
            .pointer("/userInputMessageContext/toolResults")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut answered = BTreeSet::new();
        let mut kept = Vec::new();
        for result in results {
            let id = result["toolUseId"].as_str().unwrap_or_default().to_owned();
            if calls.iter().any(|call| call["toolUseId"] == id) && answered.insert(id) {
                kept.push(result);
            }
        }
        for call in calls {
            let id = call["toolUseId"].as_str().unwrap_or_default().to_owned();
            if answered.insert(id.clone()) {
                kept.push(json!({"toolUseId":id,"status":"error","content":[{"text":"Tool use was interrupted and did not produce a result."}]}));
            }
        }
        user.as_object_mut()
            .unwrap()
            .remove("userInputMessageContext");
        if !kept.is_empty() {
            user["userInputMessageContext"] = json!({"toolResults":kept});
        }
        if user["content"] == "" && user.get("userInputMessageContext").is_none() {
            user["content"] = json!("Please proceed with the task.");
        }
    }
    let latest = entries.iter().rposition(|entry| {
        entry
            .pointer("/userInputMessage/images")
            .and_then(Value::as_array)
            .is_some_and(|images| !images.is_empty())
    });
    for (index, entry) in entries.iter_mut().enumerate() {
        if let Some(user) = entry.get_mut("userInputMessage") {
            if latest != Some(index) {
                user.as_object_mut().unwrap().remove("images");
            }
        } else if entry
            .pointer("/assistantResponseMessage/toolUses")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            entry["assistantResponseMessage"]
                .as_object_mut()
                .unwrap()
                .remove("toolUses");
        }
    }
    let mut system = text(&req["system"]);
    if let Some(format) = req
        .pointer("/output_config/format")
        .or_else(|| req.get("output_format"))
        .filter(|format| format["type"] == "json_schema" && format["schema"].is_object())
    {
        if !system.is_empty() {
            system.push_str("\n\n");
        }
        system += &format!(
            "Respond with a single JSON value and nothing else, matching this JSON schema:\n{}.",
            format["schema"]
        );
    }
    if budget > 0 {
        system=format!("<thinking_mode>enabled</thinking_mode><max_thinking_length>{budget}</max_thinking_length>\n{system}");
    }
    if !system.is_empty() {
        let old = entries[0]["userInputMessage"]["content"]
            .as_str()
            .unwrap_or_default();
        entries[0]["userInputMessage"]["content"] = json!(format!("{system}\n\n{old}"));
    }
    if parts(&req["system"])
        .iter()
        .any(|part| part.get("cache_control").is_some())
    {
        entries[0]["userInputMessage"]["cachePoint"] = json!({"type":"default"});
    }
    let mut tools = Vec::new();
    let mut offered = BTreeSet::new();
    for tool in req["tools"].as_array().into_iter().flatten() {
        let Some(original) = tool["name"].as_str().filter(|name| !name.is_empty()) else {
            continue;
        };
        if tool.get("type").is_some_and(|kind| kind != "custom")
            && tool.get("input_schema").is_none()
        {
            continue;
        }
        let wire = name(original);
        if wire != original {
            names.insert(wire.clone(), original.into());
        }
        if !offered.insert(wire.clone()) {
            continue;
        }
        tools.push(json!({"toolSpecification":{"name":wire,"description":tool["description"].as_str().filter(|description|!description.is_empty()).unwrap_or(original),"inputSchema":{"json":tool.get("input_schema").filter(|schema|schema.is_object()).cloned().unwrap_or_else(||json!({"type":"object","properties":{}}))}}}));
        if tool.get("cache_control").is_some() {
            tools.push(json!({"cachePoint":{"type":"default"}}));
        }
    }
    for entry in &entries {
        for call in entry
            .pointer("/assistantResponseMessage/toolUses")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let wire = call["name"].as_str().unwrap_or_default();
            if offered.insert(wire.into()) {
                tools.push(json!({"toolSpecification":{"name":wire,"description":"Tool","inputSchema":{"json":{"type":"object","properties":{}}}}}));
            }
        }
    }
    let mut count = tools
        .iter()
        .filter(|tool| tool.get("cachePoint").is_some())
        .count()
        + entries
            .iter()
            .filter(|entry| {
                entry
                    .get("userInputMessage")
                    .or_else(|| entry.get("assistantResponseMessage"))
                    .and_then(|message| message.get("cachePoint"))
                    .is_some()
            })
            .count();
    tools.retain(|tool| {
        if count > 4 && tool.get("cachePoint").is_some() {
            count -= 1;
            false
        } else {
            true
        }
    });
    for entry in &mut entries {
        let message = if entry.get("userInputMessage").is_some() {
            &mut entry["userInputMessage"]
        } else {
            &mut entry["assistantResponseMessage"]
        };
        if count > 4 && message.get("cachePoint").is_some() {
            message.as_object_mut().unwrap().remove("cachePoint");
            count -= 1;
        }
    }
    let mut current = entries.pop().unwrap()["userInputMessage"].take();
    if !tools.is_empty() {
        if current.get("userInputMessageContext").is_none() {
            current["userInputMessageContext"] = json!({});
        }
        current["userInputMessageContext"]["tools"] = json!(tools);
    }
    let mut state = json!({"chatTriggerType":"MANUAL","agentTaskType":"vibe","conversationId":uuid(),"currentMessage":{"userInputMessage":current}});
    if !entries.is_empty() {
        state["history"] = json!(entries);
    }
    let mut output = json!({"conversationState":state,"agentMode":"vibe"});
    if !profile.is_empty() {
        output["profileArn"] = json!(profile);
    }
    Ok((output, names))
}
#[cfg(test)]
#[path = "../test/request.test.rs"]
mod tests;
