use super::{account, network};
use nora_provider_plugin_sdk::{FetchRequest, PluginError, PluginResult};
use serde_json::{json, Value};
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) enum Wire {
    Anthropic,
    Responses,
    Chat,
    Gemini,
}
impl Wire {
    pub fn api(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic-messages",
            Self::Responses => "openai-responses",
            Self::Chat => "openai-completions",
            Self::Gemini => "google-generative-ai",
        }
    }
}
pub(crate) struct Request {
    pub wire: Wire,
    pub model: String,
    pub streaming: bool,
    pub body: Value,
}
pub(crate) fn parse(request: &FetchRequest) -> PluginResult<Request> {
    let url = url::Url::parse(&request.url)
        .map_err(|_| PluginError::invalid("The Zed request address is invalid."))?;
    if request.method != "POST"
        || url.origin() != url::Url::parse(network::CLOUD).unwrap().origin()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err(PluginError::invalid(
            "Zed only accepts requests to its declared cloud endpoint.",
        ));
    }
    let path = url.path();
    let (wire, path_model, stream) = if path == "/v1/messages" {
        (Wire::Anthropic, None, None)
    } else if path == "/v1/responses" {
        (Wire::Responses, None, None)
    } else if path == "/v1/chat/completions" {
        (Wire::Chat, None, None)
    } else if let Some(model) = path.strip_prefix("/v1/models/") {
        let (model, kind) = model
            .rsplit_once(':')
            .ok_or_else(|| PluginError::invalid("The Zed Gemini request path is invalid."))?;
        let streaming = match kind {
            "streamGenerateContent" => true,
            "generateContent" => false,
            _ => {
                return Err(PluginError::invalid(
                    "The Zed Gemini operation is unsupported.",
                ))
            }
        };
        // Model identifiers contain no path separators or encoded ambiguity.
        if model.is_empty()
            || model
                .bytes()
                .any(|b| !b.is_ascii_alphanumeric() && !b"._-".contains(&b))
        {
            return Err(PluginError::invalid("The Zed Gemini model ID is invalid."));
        }
        (Wire::Gemini, Some(model.to_owned()), Some(streaming))
    } else {
        return Err(PluginError::invalid(
            "The requested Zed protocol endpoint is unsupported.",
        ));
    };
    if !url.query_pairs().all(|(key, value)| match wire {
        Wire::Gemini => key == "key" || key == "alt" && value == "sse",
        Wire::Anthropic => key == "beta" && value == "true",
        _ => false,
    }) {
        return Err(PluginError::invalid(
            "The Zed request query is unsupported.",
        ));
    }
    let bytes = request
        .decode_body()?
        .ok_or_else(|| PluginError::invalid("The Zed request body is missing."))?;
    if bytes.len() > 32 * 1024 * 1024 {
        return Err(PluginError::invalid("The Zed request is too large."));
    }
    let mut body: Value = serde_json::from_slice(&bytes)
        .map_err(|_| PluginError::invalid("The Zed request is invalid JSON."))?;
    if !body.is_object() {
        return Err(PluginError::invalid("The Zed request must be an object."));
    }
    let model = path_model
        .or_else(|| account::text(body.get("model"), 512).map(str::to_owned))
        .ok_or_else(|| PluginError::invalid("The Zed model ID is missing."))?;
    let streaming = stream.unwrap_or(body["stream"] == true);
    match wire {
        Wire::Anthropic => {
            body.as_object_mut().unwrap().remove("stream");
            for message in body["messages"].as_array_mut().into_iter().flatten() {
                for block in message["content"].as_array_mut().into_iter().flatten() {
                    if block["type"] == "tool_result" && block.get("is_error").is_none() {
                        block["is_error"] = json!(false);
                    }
                }
            }
        }
        Wire::Responses => body["stream"] = json!(true),
        Wire::Chat => {
            body["stream"] = json!(true);
            if let Some(max) = body.as_object_mut().unwrap().remove("max_tokens") {
                body["max_completion_tokens"] = max;
            }
        }
        Wire::Gemini => {
            body.as_object_mut().unwrap().remove("session_id");
            body.as_object_mut().unwrap().remove("sessionId");
            body["model"] = json!(format!("models/{model}"));
        }
    }
    Ok(Request {
        wire,
        model,
        streaming,
        body,
    })
}
impl Request {
    pub fn envelope(&self, vendor: &str) -> Value {
        json!({"provider":vendor,"model":self.model,"provider_request":self.body})
    }
}
#[cfg(test)]
#[path = "../test/request.test.rs"]
mod tests;
