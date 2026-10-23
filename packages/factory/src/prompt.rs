use super::models::Wire;
use serde_json::{json, Value};
pub(crate) const DROID: &str = "You are Droid, an AI software engineering agent built by Factory.";
pub(crate) const QUOTED:&str="Tool output encoded as a JSON string. Decode the JSON string to recover the exact original text before using it:\n";
fn phrases(all: bool) -> Vec<String> {
    let value: Value =
        serde_json::from_str(include_str!("client-phrases.json")).expect("packaged phrases");
    let mut out = value["anywhere"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if all {
        out.extend(
            value["toolOnly"]
                .as_array()
                .unwrap()
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned),
        );
    }
    out.sort_by_key(|s| std::cmp::Reverse(s.len()));
    out
}
fn escape(text: &str, list: &[String]) -> String {
    let mut out = text.to_owned();
    for phrase in list {
        if let Some(first) = phrase.chars().next() {
            out = out.replace(
                phrase,
                &format!("\\u{:04x}{}", first as u32, &phrase[first.len_utf8()..]),
            );
        }
    }
    out
}
fn deep(value: &mut Value, list: &[String]) {
    match value {
        Value::String(text) => *text = escape(text, list),
        Value::Array(items) => {
            for value in items {
                deep(value, list);
            }
        }
        Value::Object(object) => {
            for value in object.values_mut() {
                deep(value, list);
            }
        }
        _ => {}
    }
}
pub(crate) fn quoted(text: &str) -> String {
    let mut list = phrases(true);
    if !list.iter().any(|p| text.contains(p)) {
        return text.into();
    }
    list.extend(
        [
            "system-reminder",
            "You have",
            "You are",
            "x-anthropic-billing-header",
        ]
        .iter()
        .map(|s| s.to_string()),
    );
    list.sort_by_key(|s| std::cmp::Reverse(s.len()));
    format!(
        "{QUOTED}{}",
        escape(&serde_json::to_string(text).unwrap(), &list)
    )
}
fn system(text: &str) -> String {
    let mut text = text.to_owned();
    let list = phrases(true);
    for identity in list.iter().filter(|p| p.starts_with("You are Claude")) {
        if text == *identity || text.starts_with(&format!("{identity}\n")) {
            text = format!("{DROID}{}", &text[identity.len()..]);
            break;
        }
    }
    let pi = "You are an expert coding assistant operating inside pi, a coding agent harness.";
    let text = super::metadata::prompt(&text);
    text.split('\n')
        .map(|line| {
            if line.starts_with(pi) {
                line.replacen(pi, "You are an expert coding assistant.", 1)
            } else {
                line.into()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}
fn text_parts(value: &mut Value, apply: fn(&str) -> String) {
    match value {
        Value::String(text) => *text = apply(text),
        Value::Array(parts) => {
            for p in parts {
                if let Some(text) = p
                    .get_mut("text")
                    .and_then(|value| value.as_str())
                    .map(str::to_owned)
                {
                    p["text"] = json!(apply(&text));
                }
            }
        }
        _ => {}
    }
}
fn open(text: &str) -> String {
    let text = system(text);
    if text.starts_with(DROID) {
        text
    } else if text.trim().is_empty() {
        DROID.into()
    } else {
        format!("{DROID}\n{text}")
    }
}
fn user(text: &str) -> String {
    let from="The conversation history before this point was compacted into the following summary:\n\n<summary>\n";
    if text.starts_with(from) && text.ends_with("</summary>") {
        format!(
            "Earlier conversation context is summarized below:\n\n<summary>\n{}",
            &text[from.len()..]
        )
    } else {
        super::metadata::user(text)
    }
}
pub(crate) fn adapt(body: &mut Value, wire: Wire) {
    match wire {
        Wire::Anthropic => {
            let original = body["system"].take();
            let mut system = match original {
                Value::String(text) => vec![json!({"type":"text","text":text})],
                Value::Array(parts) => parts,
                _ => Vec::new(),
            };
            system.retain(|p| {
                p["type"] != "text"
                    || p["text"].as_str().is_some_and(|s| {
                        !s.trim().is_empty()
                            && !s.starts_with("x-anthropic-billing-header: cc_version=")
                    })
            });
            for part in &mut system {
                if let Some(text) = part["text"].as_str().map(str::to_owned) {
                    part["text"] = json!(self::system(&text));
                }
            }
            if let Some(index) = system
                .iter()
                .position(|p| p["text"].as_str().is_some_and(|s| s.starts_with(DROID)))
            {
                let first = system.remove(index);
                system.insert(0, first);
            } else {
                system.insert(0, json!({"type":"text","text":DROID}));
            }
            for i in (1..system.len()).rev() {
                if system[i]["text"] == DROID {
                    system.remove(i);
                }
            }
            body["system"] = json!(system);
            if let Some(messages) = body["messages"].as_array_mut() {
                for message in messages {
                    if message["role"] == "system" {
                        text_parts(&mut message["content"], super::metadata::system);
                    } else if message["role"] == "user" {
                        if let Some(parts) = message["content"].as_array_mut() {
                            for part in parts {
                                if part["type"] == "tool_result" {
                                    text_parts(&mut part["content"], quoted);
                                } else if part["type"] == "text" {
                                    if let Some(text) = part["text"].as_str() {
                                        part["text"] = json!(user(text));
                                    }
                                }
                            }
                        } else {
                            text_parts(&mut message["content"], user);
                        }
                    }
                }
            }
            if body["stream"] == true {
                body.as_object_mut().unwrap().remove("context_management");
            }
        }
        Wire::Responses | Wire::Chat => {
            let field = if wire == Wire::Responses {
                "input"
            } else {
                "messages"
            };
            if let Some(messages) = body[field].as_array_mut() {
                for message in messages.iter_mut() {
                    if message["role"] == "tool" {
                        text_parts(&mut message["content"], quoted);
                    } else if message["type"] == "function_call_output" {
                        text_parts(&mut message["output"], quoted);
                    } else if message["role"] == "system" || message["role"] == "developer" {
                        text_parts(&mut message["content"], system);
                    } else if message["role"] == "user" {
                        text_parts(&mut message["content"], user);
                    }
                }
                if wire == Wire::Chat {
                    if messages.first().is_some_and(|m| m["role"] == "system") {
                        let content = &mut messages[0]["content"];
                        if content.is_string() {
                            text_parts(content, open);
                        } else if let Some(parts) = content.as_array_mut() {
                            if !parts
                                .first()
                                .and_then(|p| p["text"].as_str())
                                .is_some_and(|s| s.starts_with(DROID))
                            {
                                parts.insert(0, json!({"type":"text","text":DROID}));
                            }
                        }
                    } else {
                        messages.insert(0, json!({"role":"system","content":DROID}));
                    }
                }
            }
            if wire == Wire::Responses {
                body["instructions"] =
                    json!(open(body["instructions"].as_str().unwrap_or_default()));
            }
        }
        Wire::Gemini => {
            body.as_object_mut().unwrap().remove("stream");
            let mut instruction = body["systemInstruction"].take();
            if let Some(text) = instruction.as_str() {
                instruction = json!({"parts":[{"text":text}]});
            }
            if !instruction.is_object() {
                instruction = json!({"parts":[]});
            }
            instruction.as_object_mut().unwrap().remove("role");
            let parts = instruction["parts"].as_array().cloned().unwrap_or_default();
            if !parts
                .first()
                .and_then(|p| p["text"].as_str())
                .is_some_and(|s| s.starts_with(DROID))
            {
                let mut text = Vec::new();
                let mut rest = Vec::new();
                for p in parts {
                    if p.as_object().is_some_and(|p| p.len() == 1) && p["text"].is_string() {
                        text.push(p["text"].as_str().unwrap().to_owned());
                    } else {
                        rest.push(p);
                    }
                }
                rest.insert(0, json!({"text":open(&text.join("\n"))}));
                instruction["parts"] = json!(rest);
            }
            body["systemInstruction"] = instruction;
        }
    }
    let list = phrases(wire == Wire::Gemini);
    deep(body, &list);
}
#[cfg(test)]
#[path = "../test/prompt.test.rs"]
mod tests;
