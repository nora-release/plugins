use serde_json::{json, Value};
use sha2::{Digest, Sha256};

const DROPPED: &[&str] = &[
    "background",
    "conversation",
    "max_output_tokens",
    "max_tool_calls",
    "metadata",
    "moderation",
    "multi_agent",
    "prompt",
    "prompt_cache_retention",
    "safety_identifier",
    "temperature",
    "top_logprobs",
    "top_p",
    "truncation",
    "user",
    "previous_response_id",
];
const HOSTED: &[&str] = &[
    "image_generation",
    "file_search",
    "code_interpreter",
    "computer_use",
    "computer_use_preview",
    "computer",
    "mcp",
    "tool_search",
];

fn own_tools(tools: &mut Vec<Value>) {
    tools.retain(|tool| {
        !tool["type"]
            .as_str()
            .is_some_and(|kind| HOSTED.contains(&kind))
    });
}

pub(crate) fn prepare(body: &[u8]) -> Vec<u8> {
    let Ok(mut value) = serde_json::from_slice::<Value>(body) else {
        return body.to_vec();
    };
    let Some(object) = value.as_object_mut() else {
        return body.to_vec();
    };
    for key in DROPPED {
        object.remove(*key);
    }
    object.insert("store".into(), json!(false));
    object.insert("stream".into(), json!(true));
    if let Some(input) = object
        .get("input")
        .and_then(Value::as_str)
        .map(str::to_owned)
    {
        object.insert("input".into(),json!([{"type":"message","role":"user","content":[{"type":"input_text","text":input}]}]));
    }
    if let Some(input) = object.get_mut("input").and_then(Value::as_array_mut) {
        for item in input {
            if item["role"] == "system" {
                item["role"] = json!("developer");
            }
            if item["type"] == "additional_tools" {
                if let Some(tools) = item.get_mut("tools").and_then(Value::as_array_mut) {
                    own_tools(tools);
                }
            }
        }
    }
    if object
        .get("prompt_cache_key")
        .and_then(Value::as_str)
        .is_none_or(str::is_empty)
    {
        if let Some(key) = conversation_key(
            object
                .get("instructions")
                .and_then(Value::as_str)
                .unwrap_or(""),
            object
                .get("input")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
        ) {
            object.insert("prompt_cache_key".into(), json!(key));
        }
    }
    if let Some(tools) = object.get_mut("tools").and_then(Value::as_array_mut) {
        own_tools(tools);
        if tools.is_empty() {
            object.remove("tools");
            object.remove("tool_choice");
        }
    }
    serde_json::to_vec(&value).unwrap_or_else(|_| body.to_vec())
}
pub(crate) fn conversation_key(instructions: &str, input: &[Value]) -> Option<String> {
    let mut hash = Sha256::new();
    hash.update(instructions.as_bytes());
    for item in input {
        let encoded = serde_json::to_string(item)
            .ok()?
            .replace('<', "\\u003c")
            .replace('>', "\\u003e")
            .replace('&', "\\u0026")
            .replace('\u{2028}', "\\u2028")
            .replace('\u{2029}', "\\u2029");
        hash.update(encoded.as_bytes());
        if item["role"] == "user" {
            let digest = format!("{:x}", hash.finalize());
            return Some(format!(
                "{}-{}-{}-{}-{}",
                &digest[..8],
                &digest[8..12],
                &digest[12..16],
                &digest[16..20],
                &digest[20..32]
            ));
        }
    }
    None
}
pub(crate) fn explain(status: u16, body: &Value) -> Option<String> {
    let code = body
        .pointer("/error/code")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or_else(|| body.pointer("/detail/code").and_then(Value::as_str))
        .or_else(|| {
            body["detail"].as_str().and_then(|detail| {
                ["subscription_sharing_", "chatpass_v2_"]
                    .iter()
                    .find_map(|prefix| detail.find(prefix).map(|at| &detail[at..]))
                    .map(|s| {
                        s.split(|c: char| {
                            !(c == '_' || c.is_ascii_lowercase() || c.is_ascii_digit())
                        })
                        .next()
                        .unwrap_or("")
                    })
            })
        })
        .or_else(|| body.pointer("/error/type").and_then(Value::as_str))
        .unwrap_or("");
    Some(match code{
        "subscription_sharing_usage_limit_exceeded"=>"The ChatGPT plan's usage limit, shared with ChatGPT and other apps, is reached; see https://chatgpt.com/settings/usage.".into(),
        "subscription_sharing_usage_unavailable"|"subscription_sharing_user_unavailable"=>"OpenAI cannot read the account's usage right now. Try again in a moment.".into(),
        "subscription_sharing_user_not_eligible"=>"This ChatGPT account cannot use OpenAI's API because its plan is not eligible.".into(),
        "subscription_sharing_unsupported_capability"=>{let param=body.pointer("/error/param").or_else(||body.pointer("/detail/param")).and_then(Value::as_str).filter(|s|s.len()<=256&&!s.chars().any(char::is_control));param.map(|s|format!("A ChatGPT sign-in cannot use {s} through OpenAI's API.")).unwrap_or_else(||"A ChatGPT sign-in cannot use part of this request through OpenAI's API.".into())},
        "subscription_sharing_route_not_supported"=>"A ChatGPT sign-in cannot use this endpoint of OpenAI's API.".into(),
        "subscription_sharing_invalid_user"=>"OpenAI no longer accepts this ChatGPT sign-in. Sign in again.".into(),
        value if value.starts_with("chatpass_v2_")=>format!("ChatGPT refused the account ({value})."),
        _ if status==401=>"OpenAI refused this ChatGPT sign-in. Sign in again.".into(),
        _=>return None,
    })
}

#[cfg(test)]
#[path = "../test/body.test.rs"]
mod tests;
