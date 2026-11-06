use futures::stream;
use nora_ai::{HttpRequest, HttpStreamingResponse};
use serde_json::{json, Value};
use std::{collections::BTreeMap, io::Read};

pub(super) const BETA: &str = "responses_websockets=2026-02-06";
const BODY_LIMIT: usize = 512 * 1024 * 1024;

pub(super) fn message(request: &HttpRequest) -> Option<String> {
    let bytes = request.body.as_deref()?;
    let decoded;
    let bytes = match header(&request.headers, "content-encoding") {
        Some(encoding) if encoding.eq_ignore_ascii_case("zstd") => {
            let decoder = zstd::stream::read::Decoder::new(bytes).ok()?;
            let mut plain = Vec::new();
            decoder
                .take((BODY_LIMIT + 1) as u64)
                .read_to_end(&mut plain)
                .ok()?;
            decoded = plain;
            decoded.as_slice()
        }
        Some("") | None => bytes,
        Some(_) => return None,
    };
    if bytes.len() > BODY_LIMIT {
        return None;
    }
    let mut value: Value = serde_json::from_slice(bytes).ok()?;
    let object = value.as_object_mut()?;
    if object.get("stream") != Some(&Value::Bool(true)) || object.contains_key("type") {
        return None;
    }
    let mut metadata = BTreeMap::new();
    if let Some(value) = header(&request.headers, "x-codex-turn-state").filter(|v| !v.is_empty()) {
        metadata.insert("x-codex-turn-state", value);
    }
    if header(&request.headers, "x-openai-internal-codex-responses-lite")
        .is_some_and(|value| value.eq_ignore_ascii_case("true"))
    {
        metadata.insert(
            "ws_request_header_x_openai_internal_codex_responses_lite",
            "true",
        );
    }
    if !metadata.is_empty() {
        let current = object.entry("client_metadata").or_insert_with(|| json!({}));
        if current.is_null() {
            *current = json!({});
        }
        let current = current.as_object_mut()?;
        for (name, value) in metadata {
            current.entry(name).or_insert_with(|| json!(value));
        }
    }
    // Keep the request type first, as Codex does, without modifying the final
    // HTTP body kept for a possible fallback.
    let body = serde_json::to_string(&value).ok()?;
    Some(format!("{{\"type\":\"response.create\",{}", &body[1..]))
}

pub(super) fn handshake_headers(headers: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    let connection = header(headers, "connection")
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(|value| value.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    let mut result = headers
        .iter()
        .filter(|(name, _)| {
            let name = name.to_ascii_lowercase();
            !hop_header(&name)
                && !connection.contains(&name)
                && !name.starts_with("sec-websocket-")
                && !matches!(
                    name.as_str(),
                    "content-type"
                        | "content-length"
                        | "content-encoding"
                        | "accept"
                        | "accept-encoding"
                        | "x-idempotency-key"
                        | "idempotency-key"
                        | "x-codex-turn-state"
                        | "x-openai-internal-codex-responses-lite"
                        | "host"
                        | "openai-beta"
                )
        })
        .map(|(name, value)| (name.to_ascii_lowercase(), value.clone()))
        .collect::<BTreeMap<_, _>>();
    result.insert("openai-beta".into(), BETA.into());
    result
}

pub(super) fn response_headers(headers: &mut BTreeMap<String, String>) {
    let connection = header(headers, "connection")
        .into_iter()
        .flat_map(|value| value.split(','))
        .map(|value| value.trim().to_ascii_lowercase())
        .collect::<Vec<_>>();
    headers.retain(|name, _| {
        let name = name.to_ascii_lowercase();
        !hop_header(&name)
            && !connection.contains(&name)
            && !name.starts_with("sec-websocket-")
            && !matches!(
                name.as_str(),
                "content-length" | "content-type" | "content-encoding"
            )
    });
}

fn hop_header(name: &str) -> bool {
    matches!(
        name,
        "connection"
            | "keep-alive"
            | "proxy-authenticate"
            | "proxy-authorization"
            | "proxy-connection"
            | "te"
            | "trailer"
            | "transfer-encoding"
            | "upgrade"
    )
}

pub(super) fn kind(bytes: &[u8]) -> String {
    serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| value.get("type")?.as_str().map(str::to_owned))
        .unwrap_or_default()
}

pub(super) fn terminal(kind: &str) -> bool {
    matches!(
        kind,
        "response.completed"
            | "response.failed"
            | "response.incomplete"
            | "response.done"
            | "error"
    )
}

pub(super) fn sse(kind: &str, bytes: &[u8]) -> Vec<u8> {
    let compact = serde_json::from_slice::<Value>(bytes)
        .ok()
        .and_then(|value| serde_json::to_vec(&value).ok());
    let frame = compact.as_deref().unwrap_or(bytes);
    let mut out = Vec::with_capacity(kind.len() + frame.len() + 18);
    if !kind.is_empty() && !kind.contains(['\r', '\n']) {
        out.extend_from_slice(b"event: ");
        out.extend_from_slice(kind.as_bytes());
        out.push(b'\n');
    }
    for line in frame.split(|byte| *byte == b'\n') {
        out.extend_from_slice(b"data: ");
        out.extend_from_slice(line.strip_suffix(b"\r").unwrap_or(line));
        out.push(b'\n');
    }
    out.push(b'\n');
    out
}

pub(super) enum ErrorResponse {
    Fallback,
    Response(HttpStreamingResponse),
    Stream,
}

pub(super) fn error_response(headers: &BTreeMap<String, String>, frame: &[u8]) -> ErrorResponse {
    let Ok(value) = serde_json::from_slice::<Value>(frame) else {
        return ErrorResponse::Stream;
    };
    if value.pointer("/error/code").and_then(Value::as_str)
        == Some("websocket_connection_limit_reached")
    {
        return ErrorResponse::Fallback;
    }
    let status = value
        .get("status")
        .and_then(Value::as_u64)
        .filter(|status| *status != 0)
        .or_else(|| value.get("status_code").and_then(Value::as_u64));
    let Some(status) = status.filter(|status| (400..=599).contains(status)) else {
        return ErrorResponse::Stream;
    };
    let mut headers = headers.clone();
    for path in ["/headers", "/error/headers"] {
        if let Some(extra) = value.pointer(path).and_then(Value::as_object) {
            for (name, value) in extra {
                if let Some(value) = value.as_str() {
                    set_header(&mut headers, name, value.to_owned());
                }
            }
        }
    }
    response_headers(&mut headers);
    set_header(&mut headers, "content-type", "application/json".into());
    let body = match value.get("error").filter(|error| error.is_object()) {
        Some(error) => serde_json::to_vec(&json!({"error": error})).unwrap_or_default(),
        None => frame.to_vec(),
    };
    ErrorResponse::Response(HttpStreamingResponse {
        status: status as u16,
        headers,
        body: Box::pin(stream::once(async move { Ok(body.into()) })),
    })
}

pub(super) fn metadata_headers(headers: &mut BTreeMap<String, String>, frame: &[u8]) {
    let Ok(value) = serde_json::from_slice::<Value>(frame) else {
        return;
    };
    if let Some(extra) = value.get("headers").and_then(Value::as_object) {
        for (name, value) in extra {
            if let Some(value) = value.as_str() {
                missing_header(headers, name, value.to_owned());
            }
        }
    }
    response_headers(headers);
}

pub(super) fn limit_headers(headers: &mut BTreeMap<String, String>, frame: &[u8]) {
    let Ok(value) = serde_json::from_slice::<Value>(frame) else {
        return;
    };
    let name = value
        .get("metered_limit_name")
        .or_else(|| value.get("limit_name"))
        .and_then(Value::as_str)
        .map(|name| name.trim().to_lowercase().replace('_', "-"))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| "codex".into());
    if let Some(plan) = value.get("plan_type").and_then(Value::as_str) {
        missing_header(headers, "x-codex-plan-type", plan.to_owned());
    }
    for window in ["primary", "secondary"] {
        for field in [
            "used_percent",
            "window_minutes",
            "reset_at",
            "reset_after_seconds",
        ] {
            if let Some(number) = value
                .pointer(&format!("/rate_limits/{window}/{field}"))
                .filter(|value| value.is_number())
            {
                let header = format!("x-{name}-{window}-{}", field.replace('_', "-"));
                missing_header(headers, &header, number.to_string());
            }
        }
    }
    if let Some(credits) = value.get("credits") {
        for field in ["has_credits", "unlimited"] {
            if let Some(enabled) = credits.get(field).and_then(Value::as_bool) {
                missing_header(
                    headers,
                    &format!("x-codex-credits-{}", field.replace('_', "-")),
                    if enabled { "True" } else { "False" }.into(),
                );
            }
        }
        if let Some(balance) = credits.get("balance").filter(|balance| !balance.is_null()) {
            missing_header(
                headers,
                "x-codex-credits-balance",
                balance
                    .as_str()
                    .map(str::to_owned)
                    .unwrap_or_else(|| balance.to_string()),
            );
        }
    }
}

fn header<'a>(headers: &'a BTreeMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn missing_header(headers: &mut BTreeMap<String, String>, name: &str, value: String) {
    if !value.is_empty() && header(headers, name).is_none_or(|value| value.is_empty()) {
        set_header(headers, name, value);
    }
}

pub(super) fn set_header(headers: &mut BTreeMap<String, String>, name: &str, value: String) {
    headers.retain(|key, _| !key.eq_ignore_ascii_case(name));
    headers.insert(name.to_ascii_lowercase(), value);
}
