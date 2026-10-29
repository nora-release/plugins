use crate::config::Account;
use nora_ai::{AwsSigV4, HttpMethod, HttpRequest};
use nora_provider_plugin_sdk::{FetchRequest, PluginError, PluginResult};
use nora_provider_subscription_core as core;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use url::Url;

pub(crate) fn claude(model: &str) -> bool {
    model.to_ascii_lowercase().contains("anthropic.claude")
}
pub(crate) fn closed_gpt(model: &str) -> bool {
    let model = model.to_ascii_lowercase();
    model.contains("openai.gpt-") && !model.contains("gpt-oss")
}
pub(crate) fn openai_prefix(model: &str) -> bool {
    closed_gpt(model) || model.to_ascii_lowercase().starts_with("xai.")
}
pub(crate) fn route(model: &str, account: &Account, mantle: bool) -> (&'static str, String) {
    let root = account.root(mantle);
    if account.aws.is_some() {
        return ("bedrock-converse-stream", root);
    }
    if claude(model) {
        return ("anthropic-messages", format!("{root}/anthropic"));
    }
    let prefix = if !mantle || openai_prefix(model) {
        "/openai/v1"
    } else {
        "/v1"
    };
    (
        if closed_gpt(model) {
            "openai-responses"
        } else {
            "openai-completions"
        },
        format!("{root}{prefix}"),
    )
}
pub(crate) fn betas(
    values: impl IntoIterator<Item = String>,
    refused: &BTreeSet<String>,
) -> Vec<String> {
    let mut out = Vec::new();
    for value in values {
        for beta in value.split(',').map(str::trim) {
            let beta = if beta == "advanced-tool-use-2025-11-20" {
                "tool-search-tool-2025-10-19"
            } else {
                beta
            };
            if beta.is_empty()
                || beta.starts_with("oauth-")
                || [
                    "prompt-caching-scope-2026-01-05",
                    "redact-thinking-2026-02-12",
                ]
                .contains(&beta)
                || refused.contains(beta)
                || out.iter().any(|entry| entry == beta)
            {
                continue;
            }
            out.push(beta.to_owned());
        }
    }
    out
}
pub(crate) fn refused_betas(message: &str) -> BTreeSet<String> {
    let lower = message.to_ascii_lowercase();
    if !lower.contains("unexpected value")
        || !lower.contains("anthropic-beta") && !lower.contains("anthropic_beta")
    {
        return BTreeSet::new();
    }
    message
        .split('`')
        .enumerate()
        .filter(|(index, _)| index % 2 == 1)
        .map(|(_, value)| value.trim())
        .filter(|value| {
            !value.is_empty()
                && value.len() <= 128
                && value.contains('-')
                && *value != "anthropic-beta"
                && *value != "anthropic_beta"
        })
        .take(32)
        .map(str::to_owned)
        .collect()
}
pub(crate) fn prepare(
    request: &FetchRequest,
    account: &Account,
    mantle: bool,
    other: bool,
    refused: &BTreeSet<String>,
) -> PluginResult<HttpRequest> {
    let mut target = Url::parse(&request.url)
        .map_err(|_| PluginError::invalid("The Bedrock request address is invalid."))?;
    let root = Url::parse(&account.root(mantle))
        .map_err(|_| PluginError::invalid("The Bedrock region is invalid."))?;
    let path = target.path().to_owned();
    let converse = account.aws.is_some()
        && path.starts_with("/model/")
        && (path.ends_with("/converse") || path.ends_with("/converse-stream"));
    let messages = path == "/anthropic/v1/messages";
    let completions =
        path == "/openai/v1/chat/completions" || mantle && path == "/v1/chat/completions";
    let responses = path == "/openai/v1/responses" || mantle && path == "/v1/responses";
    if request.method != "POST"
        || target.origin() != root.origin()
        || !target.username().is_empty()
        || target.password().is_some()
        || target.fragment().is_some()
        || target
            .query_pairs()
            .any(|(key, value)| !messages || key != "beta" || value != "true")
        || !messages && !completions && !responses && !converse
    {
        return Err(PluginError::invalid(
            "This request is outside this account's Bedrock endpoints.",
        ));
    }
    let mut body = core::request_body(request)?;
    if !body.is_object() {
        return Err(PluginError::invalid(
            "The Bedrock request must be a JSON object.",
        ));
    }
    let model = body
        .get("model")
        .and_then(core::text)
        .unwrap_or("")
        .to_owned();
    if !converse && (model.is_empty() || model.len() > 1024 || model.chars().any(char::is_control))
    {
        return Err(PluginError::invalid(
            "The Bedrock model is missing or invalid.",
        ));
    }
    if mantle && (completions || responses) {
        let suffix = if completions {
            "chat/completions"
        } else {
            "responses"
        };
        target.set_path(&format!(
            "{}/{suffix}",
            if openai_prefix(&model) != other {
                "/openai/v1"
            } else {
                "/v1"
            }
        ));
    }
    let mut headers = BTreeMap::from([("content-type".into(), "application/json".into())]);
    for (name, value) in &request.headers {
        if [
            "accept",
            "anthropic-version",
            "x-stainless-lang",
            "x-stainless-package-version",
        ]
        .contains(&name.to_ascii_lowercase().as_str())
        {
            headers.insert(name.clone(), value.clone());
        }
    }
    if messages {
        if let Some(metadata) = body.get_mut("metadata").and_then(Value::as_object_mut) {
            metadata.remove("user_id");
            if metadata.is_empty() {
                body.as_object_mut().unwrap().remove("metadata");
            }
        }
        let asked = request
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("anthropic-beta"))
            .map(|(_, value)| value.clone());
        let fitted = betas(asked, refused);
        if !fitted.is_empty() {
            headers.insert("anthropic-beta".into(), fitted.join(","));
        }
        if let Some(asked) = body.get("anthropic_beta").and_then(Value::as_array) {
            let fitted = betas(
                asked.iter().filter_map(Value::as_str).map(str::to_owned),
                refused,
            );
            if fitted.is_empty() {
                body.as_object_mut().unwrap().remove("anthropic_beta");
            } else {
                body["anthropic_beta"] = json!(fitted);
            }
        }
    }
    if completions {
        if let Some(max) = body.as_object_mut().unwrap().remove("max_tokens") {
            body.as_object_mut()
                .unwrap()
                .entry("max_completion_tokens")
                .or_insert(max);
        }
    }
    let bytes = serde_json::to_vec(&body)
        .map_err(|_| PluginError::invalid("The Bedrock request cannot be encoded."))?;
    if let Some(aws) = &account.aws {
        headers = AwsSigV4::sign_post(
            target.as_str(),
            &bytes,
            &account.region,
            "bedrock",
            aws,
            headers,
        );
    } else if messages {
        headers.insert("x-api-key".into(), account.key.clone());
    } else {
        headers.insert("authorization".into(), format!("Bearer {}", account.key));
    }
    let mut output = HttpRequest::new(HttpMethod::Post, target.as_str());
    output.headers = headers;
    output.body = Some(bytes);
    Ok(output)
}

#[cfg(test)]
#[path = "../test/request.test.rs"]
mod tests;
