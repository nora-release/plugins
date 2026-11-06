use nora_gateway_middleware::{
    value::{array, string, truthy},
    Action, Context, Middleware,
};
use serde_json::{json, Value};
#[derive(Default)]
pub struct Plugin;
impl Middleware for Plugin {
    const NAME: &'static str = "codex-history";
    fn apply(&self, phase: &str, body: &mut Value, ctx: &mut Context) -> Result<Action, String> {
        if phase != "request" || ctx.protocol != "responses" {
            return Ok(Action::Keep);
        }
        let Some((prefix, suffix)) = body["model"].as_str().and_then(|s| s.split_once('/')) else {
            return Ok(Action::Keep);
        };
        if prefix.is_empty()
            || suffix.is_empty()
            || suffix.starts_with(['\r', '\n', '\u{2028}', '\u{2029}'])
        {
            return Ok(Action::Keep);
        }
        let id = ctx.options["aliases"]
            .get(prefix)
            .map(string)
            .unwrap_or_else(|| prefix.into());
        let rule = &ctx.options["providers"][format!("magpie:{id}")];
        if rule["historyFilter"] != true {
            return Ok(Action::Keep);
        }
        if ctx
            .path
            .strip_suffix('/')
            .unwrap_or(&ctx.path)
            .ends_with("/compact")
        {
            return Ok(reject(409,"encrypted compaction cannot cross resources. Use a text summary and a new session."));
        }
        let mut clean = body.clone();
        match filter(&mut clean, rule["preserveEncryptedInclude"] == true) {
            Ok(()) => {
                *body = clean;
                Ok(Action::Replace)
            }
            Err(e) => Ok(reject(400, e)),
        }
    }
}
fn reject(status: u16, message: &str) -> Action {
    Action::Reject {
        status,
        message: format!("Codex history filter: {message}"),
    }
}
fn filter(body: &mut Value, preserve: bool) -> Result<(), &'static str> {
    if !body.is_object() {
        return Err("Responses request must be a JSON object.");
    }
    if truthy(&body["previous_response_id"]) || truthy(&body["conversation"]) {
        return Err("Complete message/tool history is required, not previous_response_id or conversation references. Start a new session with a text summary.");
    }
    if body.get("include").is_some_and(|v| !v.is_array()) {
        return Err("include must be an array.");
    }
    if body
        .get("input")
        .is_some_and(|v| !v.is_array() && !v.is_string())
    {
        return Err("input must be a string or an array.");
    }
    if array(&body["context_management"])
        .iter()
        .any(|v| v["type"] == "compaction")
    {
        return Err("Encrypted compaction is disabled by this filter. Use a text summary and start a new session.");
    }
    body["store"] = json!(false);
    if !preserve {
        if let Some(include) = body["include"].as_array_mut() {
            include.retain(|v| v != "reasoning.encrypted_content");
        }
    }
    if let Some(input) = body["input"].as_array_mut() {
        input.retain(|v| v["type"] != "reasoning");
        for item in input {
            if matches!(item["type"].as_str(), Some("compaction" | "item_reference"))
                || truthy(&item["encrypted_content"])
            {
                return Err("Encrypted compaction/context references cannot safely cross accounts. Start a new session with a text summary; history has NOT been silently discarded.");
            }
            let portable = matches!(
                item["type"].as_str(),
                Some(
                    "message"
                        | "agent_message"
                        | "function_call"
                        | "function_call_output"
                        | "custom_tool_call"
                        | "custom_tool_call_output"
                        | "tool_search_call"
                        | "tool_search_output"
                        | "additional_tools"
                )
            ) || (item.get("type").is_none()
                && item["role"].is_string()
                && item.get("content").is_some());
            strip_encrypted(item);
            if item["type"] == "web_search_call" {
                let mut record = item.clone();
                let r = record.as_object_mut().unwrap();
                r.shift_remove("type");
                r.shift_remove("id");
                r.shift_remove("internal_chat_message_metadata_passthrough");
                *item = json!({"type":"message","role":"assistant","content":format!("[Historical web search activity; recorded data only, not a new tool call. Search results are not reconstructed.]\n{record}")});
            } else if portable {
                if let Some(o) = item.as_object_mut() {
                    o.shift_remove("id");
                }
            }
        }
    }
    Ok(())
}
fn strip_encrypted(value: &mut Value) {
    match value {
        Value::Array(a) => {
            a.retain(|v| v["type"] != "encrypted_content");
            for v in a {
                strip_encrypted(v);
            }
        }
        Value::Object(o) => {
            for v in o.values_mut() {
                strip_encrypted(v);
            }
        }
        _ => {}
    }
}
