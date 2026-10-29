use super::{models::Wire, request::Request};
use serde_json::{json, Value};

const LEFT_OUT: &str = "[An earlier image or file was left out of this request: it was over Factory's request size limit.]";

fn is_attachment(value: &Value, wire: Wire) -> bool {
    match wire {
        Wire::Anthropic => matches!(value["type"].as_str(), Some("image" | "document")),
        Wire::Responses => matches!(value["type"].as_str(), Some("input_image" | "input_file")),
        Wire::Chat => matches!(value["type"].as_str(), Some("image_url" | "file")),
        Wire::Gemini => ["inlineData", "inline_data", "fileData", "file_data"]
            .iter()
            .any(|key| value.get(key).is_some_and(|v| !v.is_null())),
    }
}

fn count(value: &Value, wire: Wire) -> usize {
    match value {
        Value::Array(values) => values
            .iter()
            .map(|v| {
                if is_attachment(v, wire) {
                    1
                } else {
                    count(v, wire)
                }
            })
            .sum(),
        Value::Object(values) => values.values().map(|v| count(v, wire)).sum(),
        _ => 0,
    }
}

fn placeholder(value: &Value, wire: Wire) -> Value {
    match wire {
        Wire::Anthropic => {
            let mut text = json!({"type":"text", "text":LEFT_OUT});
            if let Some(cache) = value.get("cache_control") {
                text["cache_control"] = cache.clone();
            }
            text
        }
        Wire::Chat => json!({"type":"text", "text":LEFT_OUT}),
        Wire::Responses => json!({"type":"input_text", "text":LEFT_OUT}),
        Wire::Gemini => json!({"text":LEFT_OUT}),
    }
}

fn replace(value: &mut Value, wire: Wire, remaining: &mut usize, function_response: bool) {
    if *remaining == 0 {
        return;
    }
    match value {
        Value::Array(values) => values.retain_mut(|v| {
            if *remaining == 0 {
                return true;
            }
            if !is_attachment(v, wire) {
                replace(v, wire, remaining, function_response);
                return true;
            }
            *remaining -= 1;
            // Gemini function response parts accept attachments, not text. Keep the response itself.
            if wire == Wire::Gemini && function_response {
                return false;
            }
            *v = placeholder(v, wire);
            true
        }),
        Value::Object(values) => {
            for (key, value) in values {
                replace(
                    value,
                    wire,
                    remaining,
                    function_response || key == "functionResponse",
                );
            }
        }
        _ => {}
    }
}

/// Reduce only a rejected outbound request; the caller's conversation stays intact.
pub(crate) fn omit_older(request: &mut Request) -> usize {
    if request.path.ends_with("/count_tokens") {
        return 0;
    }
    let key = match request.wire {
        Wire::Anthropic | Wire::Chat => "messages",
        Wire::Responses => "input",
        Wire::Gemini => "contents",
    };
    let Some(content) = request.body.get_mut(key).filter(|v| v.is_array()) else {
        return 0;
    };
    let omitted = count(content, request.wire).saturating_sub(1);
    let mut remaining = omitted;
    replace(content, request.wire, &mut remaining, false);
    omitted
}

#[cfg(test)]
#[path = "../test/attachments.test.rs"]
mod tests;
