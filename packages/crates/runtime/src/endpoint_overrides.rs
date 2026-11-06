use crate::{
    auth,
    balance_sources::{self, Reader, Source},
    definition::Definition,
};
use nora_ai::{HttpClient, HttpMethod, HttpRequest, ResolvedProviderAuth};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::{collections::BTreeMap, sync::OnceLock, time::Duration};

pub(crate) fn validate(definition: &Definition) -> PluginResult<()> {
    for value in [
        &definition.config.models_url,
        &definition.config.balance_url,
    ]
    .into_iter()
    .filter(|value| !value.trim().is_empty())
    {
        // Placeholder values must be escaped before parsing a complete URL.
        crate::discovery::validated_url(&replace_key(value, "key", true))?;
    }
    if !matches!(
        definition
            .config
            .balance_method
            .trim()
            .to_ascii_uppercase()
            .as_str(),
        "" | "GET" | "POST"
    ) {
        return Err(PluginError::invalid(
            "The balance request method must be GET or POST.",
        ));
    }
    if definition.config.balance_token.contains(['\r', '\n'])
        || definition.config.balance_path.len() > 16_384
    {
        return Err(PluginError::invalid(
            "The balance request settings are invalid.",
        ));
    }
    if definition.config.insecure_tls && !crate::network_client::takes_unchecked_tls(definition) {
        return Err(PluginError::invalid(
            "Unchecked TLS certificates are only available for a custom API provider.",
        ));
    }
    Ok(())
}

pub(crate) fn applies(definition: &Definition, credentials: &Value) -> bool {
    if !auth::is_api(credentials) {
        return false;
    }
    if !definition.config.balance_url.trim().is_empty() {
        return true;
    }
    let known = balance_sources::known(definition);
    known.as_ref().is_some_and(|source| {
        definition.adapter.id().is_none()
            || (!definition.config.balance_token.trim().is_empty()
                && source.url == "https://aihubmix.com/api/user/self")
    })
}

pub(crate) async fn balance(
    definition: &Definition,
    credentials: &Value,
    resolved: &ResolvedProviderAuth,
    client: &dyn HttpClient,
) -> PluginResult<Value> {
    let key = resolved.token.as_deref().unwrap_or_default();
    let normalized = if !definition.config.balance_token.is_empty()
        && account_token(&definition.config.balance_token, key).is_none()
    {
        let mut normalized = definition.clone();
        normalized.config.balance_token.clear();
        Some(normalized)
    } else {
        None
    };
    let source_definition = normalized.as_ref().unwrap_or(definition);
    let mut source = if definition.config.balance_url.trim().is_empty() {
        balance_sources::known(source_definition)
            .ok_or_else(|| PluginError::unsupported("balance"))?
    } else {
        Source {
            url: definition.config.balance_url.trim().into(),
            path: definition.config.balance_path.trim().into(),
            reader: Reader::Path,
        }
    };
    let token = account_token(&definition.config.balance_token, key);
    let mut user_headers = definition.config.headers.clone();
    crate::adapter::merge_headers(
        &mut user_headers,
        crate::account_headers::read(credentials)?,
    );
    let own_auth = user_headers.iter().any(|(name, value)| {
        !value.trim().is_empty()
            && (name.eq_ignore_ascii_case("authorization") || name.eq_ignore_ascii_case("cookie"))
    });
    if token.is_none() && !own_auth {
        key_query(&mut source)?;
    }
    let mut url = crate::discovery::validated_url(&replace_key(&source.url, key, true))?;
    let path = url.path().trim_end_matches('/');
    if token.is_some() && path.ends_with("/api/usage/token") {
        return Err(PluginError::invalid("The key usage endpoint accepts an API key. Set the balance URL to /api/user/self for the account balance token."));
    }
    if token.is_some() && path.ends_with("/v1/usage") {
        return Err(PluginError::invalid("The key usage endpoint accepts an API key. Set the balance URL to /api/v1/user/profile for the account balance token."));
    }
    if source.path.is_empty() && matches!(source.reader, Reader::Path) {
        source.path = default_path(path).into();
    }
    if matches!(source.reader, Reader::Path) && source.path.trim().is_empty() {
        return Err(PluginError::invalid(
            "Choose where in the balance response its amount is, for example data.balance.",
        ));
    }
    let mut headers = BTreeMap::new();
    if token.is_none() {
        nora_ai::auth_headers::apply_resolved_auth_with_merge(
            resolved,
            &mut headers,
            |headers, name, value| {
                headers.retain(|key, _| !key.eq_ignore_ascii_case(&name));
                headers.insert(name, value);
            },
        );
        if let nora_ai::AuthScheme::QueryKey { name } = &resolved.scheme {
            url.query_pairs_mut().append_pair(name, key);
        }
        crate::auth_headers::apply(
            definition,
            credentials,
            &definition.api,
            url.as_str(),
            key,
            &mut headers,
        );
    }
    for (name, value) in &user_headers {
        crate::adapter::merge_headers(
            &mut headers,
            [(name.clone(), replace_key(value, key, false))],
        );
    }
    if let Some(token) = token {
        crate::adapter::merge_headers(
            &mut headers,
            [("Authorization".into(), authorization(token))],
        );
    }
    crate::adapter::merge_headers(&mut headers, [("Accept".into(), "application/json".into())]);
    let method = if !definition.config.balance_url.trim().is_empty()
        && definition
            .config
            .balance_method
            .trim()
            .eq_ignore_ascii_case("POST")
    {
        HttpMethod::Post
    } else {
        HttpMethod::Get
    };
    let request = HttpRequest {
        method,
        url: url.to_string(),
        headers,
        body: None,
    };
    let reply = crate::usage::request_json(client, request, Duration::from_secs(30))
        .await
        .map_err(PluginError::transport)?;
    if reply.get("success") == Some(&Value::Bool(false)) {
        return Err(PluginError::transport(
            "The provider rejected the balance request.",
        ));
    }
    let mut parts = balance_sources::read(&source, &reply)?;
    let mut secrets = vec![
        key.to_string(),
        definition.config.balance_token.trim().to_string(),
        source.url.clone(),
    ];
    secrets.extend(url.query_pairs().map(|(_, value)| value.into_owned()));
    secrets.extend(user_headers.values().cloned());
    let tokens = secrets
        .iter()
        .filter_map(|value| {
            value
                .split_once(char::is_whitespace)
                .filter(|(scheme, _)| {
                    ["bearer", "basic", "token"]
                        .iter()
                        .any(|known| scheme.eq_ignore_ascii_case(known))
                })
                .map(|(_, token)| token.trim().to_string())
        })
        .collect::<Vec<_>>();
    secrets.extend(tokens);
    let escaped = secrets
        .iter()
        .map(|secret| url::form_urlencoded::byte_serialize(secret.as_bytes()).collect::<String>())
        .collect::<Vec<_>>();
    secrets.extend(escaped);
    secrets.sort_by_key(|secret| std::cmp::Reverse(secret.len()));
    for part in &mut parts {
        redact(&mut part.label, &secrets);
        redact(&mut part.text, &secrets);
        if let Some(error) = part.error.as_mut() {
            redact(error, &secrets);
        }
    }
    let mut items = Vec::new();
    let mut balances = Vec::new();
    for (index, part) in parts.iter().enumerate() {
        let id = format!("balance-{index}");
        let label = if part.label.is_empty() {
            "Balance"
        } else {
            &part.label
        };
        if let Some(amount) = part.amount.filter(|_| part.percent.is_none()) {
            balances.push(json!({"id":id,"label":label,"amount":amount,"unit":part.currency.unwrap_or(""),"currency":part.currency.unwrap_or("")}));
        } else {
            let mut item =
                json!({"id":id,"label":label,"value":part.text,"countsForRouting":false});
            if let Some(percent) = part.percent {
                item["usedPercent"] = json!(percent);
            }
            if let Some(error) = &part.error {
                item["error"] = json!(error);
            }
            items.push(item);
        }
    }
    let mut usage = crate::usage::parsing::result(&json!({}), items, balances);
    usage["balanceParts"] = json!(parts);
    Ok(usage)
}

fn account_token<'a>(token: &'a str, key: &str) -> Option<&'a str> {
    let token = token.trim();
    let stripped = token
        .split_once(' ')
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, value)| value.trim());
    (!token.is_empty() && token != key && stripped != Some(key)).then_some(token)
}
fn authorization(token: &str) -> String {
    if !token.contains(' ') && token.starts_with("eyJ") && token.matches('.').count() == 2 {
        format!("Bearer {token}")
    } else {
        token.into()
    }
}
fn key_query(source: &mut Source) -> PluginResult<()> {
    let mut url = crate::discovery::validated_url(&source.url)?;
    let path = url.path().trim_end_matches('/');
    for (account, key, field) in [
        ("/api/user/self", "/api/usage/token", "$data.quota / 500000"),
        ("/api/v1/user/profile", "/v1/usage", "$data.balance"),
    ] {
        if let Some(prefix) = path.strip_suffix(account) {
            url.set_path(&format!("{prefix}{key}"));
            url.set_query(None);
            url.set_fragment(None);
            source.url = url.into();
            if source.path.trim() == field {
                source.path.clear();
            }
            break;
        }
    }
    Ok(())
}
fn default_path(path: &str) -> &'static str {
    match path {
        "/api/user/self" => "$data.quota / 500000",
        "/api/usage/token" => "$data.total_available / 500000",
        "/api/v1/user/profile" => "$data.balance",
        "/v1/usage" => "$remaining",
        path if path.ends_with("/dashboard/billing/credit_grants") => "$total_available",
        // keyQueryOf preserves a panel's optional prefix.
        path if path.ends_with("/api/usage/token") => "$data.total_available / 500000",
        path if path.ends_with("/v1/usage") => "$remaining",
        _ => "",
    }
}
fn replace_key(value: &str, key: &str, query: bool) -> String {
    static PLACEHOLDER: OnceLock<regex::Regex> = OnceLock::new();
    let regex = PLACEHOLDER.get_or_init(|| {
        regex::Regex::new(r"(?i)\$?\{\{?\s*(?:key|api[_-]?key|token)\s*\}?\}").unwrap()
    });
    let encoded = if query {
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("", key)
            .finish()[1..]
            .to_string()
    } else {
        key.to_string()
    };
    regex
        .replace_all(value, regex::NoExpand(&encoded))
        .into_owned()
}
fn redact(value: &mut String, secrets: &[String]) {
    for secret in secrets.iter().filter(|secret| !secret.is_empty()) {
        *value = value.replace(secret, "[redacted]");
    }
}

#[cfg(test)]
#[path = "../test/endpoint_overrides.test.rs"]
mod tests;
