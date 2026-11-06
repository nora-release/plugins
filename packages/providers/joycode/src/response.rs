use crate::*;
use futures::stream;
fn failure(status: u16, code: Value, message: &str) -> FetchResponse {
    FetchResponse::bytes(
        status,
        BTreeMap::from([
            ("content-type".into(), "application/json".into()),
            (
                "x-magpie-sign-in".into(),
                if status == 401 { "expired" } else { "kept" }.into(),
            ),
        ]),
        json!({"error":{"type":"upstream_error","code":code,"message":message}})
            .to_string()
            .into_bytes(),
    )
}
fn redact(mut value: String, secrets: &[String]) -> String {
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        value = value.replace(secret, "[redacted]");
    }
    value
}
pub(crate) async fn normalize(
    mut response: HttpStreamingResponse,
    secrets: &[String],
) -> PluginResult<FetchResponse> {
    let mut headers = response.headers;
    headers.retain(|key, _| {
        !matches!(
            key.to_ascii_lowercase().as_str(),
            "content-length" | "content-encoding"
        )
    });
    if !(200..300).contains(&response.status) {
        let body = tokio::time::timeout(
            Duration::from_secs(30),
            nora_ai::collect_http_body_text_limited(response.body, 1024 * 1024),
        )
        .await
        .map_err(|_| transport("JoyCode error response timed out."))?
        .map_err(|_| transport("JoyCode error response was incomplete."))?;
        headers.insert(
            "x-magpie-sign-in".into(),
            if response.status == 401 {
                "expired"
            } else {
                "kept"
            }
            .into(),
        );
        return Ok(FetchResponse::bytes(
            response.status,
            headers,
            redact(body, secrets).into_bytes(),
        ));
    }
    let mut prefix = Vec::new();
    loop {
        let text = String::from_utf8_lossy(&prefix);
        if !text.trim().is_empty() {
            break;
        }
        if prefix.len() > 65536 {
            return Ok(failure(
                502,
                json!("empty_response"),
                "JoyCode returned an empty response.",
            ));
        }
        let next = tokio::time::timeout(Duration::from_secs(30), response.body.next())
            .await
            .map_err(|_| transport("JoyCode response timed out."))?;
        let Some(next) = next else {
            return Ok(failure(
                502,
                json!("empty_response"),
                "JoyCode returned an empty response.",
            ));
        };
        let next = next.map_err(|_| transport("JoyCode response was interrupted."))?;
        prefix.extend_from_slice(&next);
    }
    if String::from_utf8_lossy(&prefix)
        .trim_start()
        .starts_with('{')
    {
        if prefix.len() > 1024 * 1024 {
            return Ok(failure(
                502,
                json!("invalid_response"),
                "JoyCode returned oversized JSON.",
            ));
        }
        while let Some(next) = tokio::time::timeout(Duration::from_secs(30), response.body.next())
            .await
            .map_err(|_| transport("JoyCode response timed out."))?
        {
            let next = next.map_err(|_| transport("JoyCode response was interrupted."))?;
            if prefix.len() + next.len() > 1024 * 1024 {
                return Ok(failure(
                    502,
                    json!("invalid_response"),
                    "JoyCode returned oversized JSON.",
                ));
            }
            prefix.extend_from_slice(&next);
        }
        let value: Value = serde_json::from_slice(&prefix).unwrap_or(Value::Null);
        if value.get("error").is_some()
            || value
                .get("code")
                .is_some_and(|code| code_number(Some(code)) != Some(0.0))
        {
            let code = value
                .pointer("/error/code")
                .or_else(|| value.get("code"))
                .cloned()
                .unwrap_or(Value::Null);
            let status = if code == "AI_GRAY_ACCESS_DENIED" {
                403
            } else if code_number(Some(&code)) == Some(401.0) {
                401
            } else {
                502
            };
            let message = value
                .pointer("/error/message")
                .or_else(|| value.get("message"))
                .or_else(|| value.get("msg"))
                .and_then(Value::as_str)
                .map(|message| message.chars().take(500).collect::<String>())
                .unwrap_or_else(|| "JoyCode refused the request.".into());
            let safe_code = match code {
                Value::String(code) => json!(redact(code.chars().take(500).collect(), secrets)),
                Value::Number(_) => code,
                _ => json!("upstream_error"),
            };
            return Ok(failure(status, safe_code, &redact(message, secrets)));
        }
        if value["choices"].is_array() {
            headers.insert("content-type".into(), "application/json".into());
            return Ok(FetchResponse::bytes(response.status, headers, prefix));
        }
        return Ok(failure(
            502,
            json!("invalid_response"),
            "JoyCode did not return a chat response.",
        ));
    }
    Ok(FetchResponse {
        status: response.status,
        headers,
        body: Box::pin(
            stream::once(async move { Ok(prefix) }).chain(response.body.map(|chunk| {
                chunk
                    .map(|chunk| chunk.to_vec())
                    .map_err(|_| transport("JoyCode stream was interrupted."))
            })),
        ),
    })
}
