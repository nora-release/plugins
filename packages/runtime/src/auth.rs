use crate::definition::Definition;
use nora_ai::{AuthScheme, JsonValue, OAuthCredentials, OAuthProviderId, ResolvedProviderAuth};
use nora_provider_plugin_sdk::{NetworkProxy, PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeMap;

pub fn is_api(value: &Value) -> bool {
    ["type", "authMethod"].iter().any(|key| {
        value
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|kind| matches!(kind, "api" | "apiKey"))
    }) || value.get("key").is_some()
}

pub fn provider_id<'a>(definition: &'a Definition, value: &Value) -> &'a str {
    if is_api(value) {
        &definition.config.key_provider
    } else {
        definition
            .config
            .oauth_provider
            .as_deref()
            .unwrap_or(&definition.config.key_provider)
    }
}

pub fn decode(definition: &Definition, value: Option<&Value>) -> PluginResult<OAuthCredentials> {
    let original =
        value.ok_or_else(|| PluginError::authentication("Connect an account to this provider."))?;
    let mut value = original.clone();
    if value.get("access").is_none() && value.get("accessToken").is_none() {
        value["access"] = original.get("key").cloned().unwrap_or_else(|| json!(""));
    }
    let fallback = if is_api(original) {
        "apiKey"
    } else {
        definition
            .config
            .legacy
            .get("credentialProvider")
            .and_then(Value::as_str)
            .unwrap_or("apiKey")
    };
    value["provider"] = original
        .get("credentialProvider")
        .or_else(|| original.get("provider"))
        .cloned()
        .unwrap_or_else(|| json!(fallback));
    let mut credentials: OAuthCredentials = serde_json::from_value(value).map_err(|_| {
        PluginError::authentication("The saved provider account is invalid. Sign in again.")
    })?;
    for key in ["email", "name", "plan"] {
        if credentials
            .extras
            .get(key)
            .and_then(|value| value.as_str())
            .is_none_or(|value| value.trim().is_empty())
        {
            if let Some(value) = original
                .get(key)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|value| {
                    !value.is_empty() && value.len() <= 512 && !value.chars().any(char::is_control)
                })
            {
                credentials
                    .extras
                    .insert(key.into(), JsonValue::string(value));
            }
        }
    }
    if !credentials.extras.contains_key("accountIdentity") {
        if let Some(identity) = original
            .get("accountIdentity")
            .filter(|value| value.is_object())
        {
            credentials
                .extras
                .insert("accountIdentity".into(), identity.clone().into());
        }
    }
    definition
        .adapter
        .enrich_credentials(original, &mut credentials);
    if credentials.access_token.trim().is_empty()
        && !definition.config.key_optional
        && !definition.adapter.is_source(&credentials)
    {
        return Err(PluginError::authentication(
            "The saved provider account has no credentials.",
        ));
    }
    Ok(credentials)
}

pub fn encode(credentials: &OAuthCredentials, auth_provider: &str, source: bool) -> Value {
    let mut value = serde_json::to_value(credentials).unwrap_or_else(|_| json!({}));
    let provider = serde_json::to_value(&credentials.provider).unwrap_or_else(|_| json!("apiKey"));
    value["provider"] = provider.clone();
    value["credentialProvider"] = provider;
    let api = matches!(
        credentials.provider,
        OAuthProviderId::ApiKey | OAuthProviderId::OpenAICompatible
    );
    let source_value = credentials
        .extras
        .get("noraAccountSource")
        .map(|value| value.as_value().clone());
    let source_kind = source_value
        .as_ref()
        .and_then(|value| value.get("kind"))
        .and_then(Value::as_str)
        .unwrap_or("external");
    value["type"] = json!(if source {
        source_kind
    } else if api {
        "apiKey"
    } else {
        "oauth"
    });
    value["authMethod"] = json!(if source {
        source_kind
    } else if api {
        "apiKey"
    } else {
        "oauth"
    });
    value["authProvider"] = json!(auth_provider);
    if source {
        value["access"] = json!("");
        value["refresh"] = Value::Null;
        value["expires"] = Value::Null;
    }
    for key in ["accountId", "userId", "email", "name", "plan"] {
        if let Some(text) = credentials.extras.get(key).and_then(|value| value.as_str()) {
            value[key] = json!(text);
        }
    }
    if let Some(identity) = credentials.extras.get("accountIdentity") {
        value["accountIdentity"] = identity.as_value().clone();
    }
    let label = ["email", "name"]
        .into_iter()
        .find_map(|key| {
            credentials
                .extras
                .get(key)
                .and_then(|value| value.as_str())
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
        .unwrap_or(if source {
            "Connected account"
        } else if api {
            "API key"
        } else {
            "Account"
        });
    value["label"] = json!(label);
    value
}

pub fn public_profile(credentials: &OAuthCredentials) -> Value {
    let profile = ["email", "name", "plan"]
        .into_iter()
        .filter_map(|key| {
            let text = credentials
                .extras
                .get(key)
                .and_then(|value| value.as_str())?
                .trim();
            (!text.is_empty() && text.len() <= 512 && !text.chars().any(char::is_control))
                .then(|| (key.to_string(), json!(text)))
        })
        .collect::<serde_json::Map<_, _>>();
    Value::Object(profile)
}

pub fn base_auth(provider_id: &str, credentials: &OAuthCredentials) -> ResolvedProviderAuth {
    let text = |key: &str| credentials.extras.get(key).and_then(|value| value.as_str());
    let scheme = match text("noraAuthScheme") {
        Some("none") => AuthScheme::None,
        Some("query-key") => AuthScheme::QueryKey {
            name: text("noraAuthHeaderName").unwrap_or("key").into(),
        },
        Some("api-key-header") => AuthScheme::ApiKeyHeader {
            name: text("noraAuthHeaderName").unwrap_or("x-api-key").into(),
        },
        Some("bearer") => AuthScheme::Bearer,
        _ => AuthScheme::Bearer,
    };
    let headers = credentials
        .extras
        .get("noraAuthHeaders")
        .and_then(JsonValue::object_value)
        .map(|headers| {
            headers
                .iter()
                .filter_map(|(key, value)| value.as_str().map(|value| (key.clone(), value.into())))
                .collect()
        })
        .unwrap_or_default();
    let auth = ResolvedProviderAuth {
        token: (!credentials.access_token.is_empty()).then(|| credentials.access_token.clone()),
        scheme,
        headers,
        base_url: text("baseUrl")
            .or_else(|| text("endpoint"))
            .map(str::to_string),
        metadata: BTreeMap::new(),
        credential_provider_id: Some(provider_id.into()),
    };
    auth
}

pub async fn resolve(
    definition: &Definition,
    value: &Value,
) -> PluginResult<(OAuthCredentials, ResolvedProviderAuth)> {
    resolve_with_network(definition, value, None).await
}

pub async fn resolve_with_network(
    definition: &Definition,
    value: &Value,
    proxy: Option<&NetworkProxy>,
) -> PluginResult<(OAuthCredentials, ResolvedProviderAuth)> {
    let credentials = decode(definition, Some(value))?;
    let mut auth = if definition.adapter.is_source(&credentials) {
        definition
            .adapter
            .resolve_source(&credentials, proxy)
            .await?
    } else {
        let mut auth = base_auth(provider_id(definition, value), &credentials);
        definition.adapter.enrich_auth(&credentials, &mut auth);
        auth
    };
    if is_api(value) {
        auth.scheme = definition.config.api_key_auth.clone().unwrap_or_else(|| {
            match definition.api.as_str() {
                "anthropic-messages" => AuthScheme::ApiKeyHeader {
                    name: "x-api-key".into(),
                },
                "google-generative-ai" => AuthScheme::QueryKey { name: "key".into() },
                "azure-openai-responses" => AuthScheme::ApiKeyHeader {
                    name: "api-key".into(),
                },
                _ => AuthScheme::Bearer,
            }
        });
        auth.base_url = Some(crate::models::effective_endpoint(definition, Some(value)));
    }
    crate::adapter::merge_headers(&mut auth.headers, crate::account_headers::read(value)?);
    Ok((credentials, auth))
}

#[cfg(test)]
#[path = "../test/auth.test.rs"]
mod tests;
#[cfg(test)]
pub(crate) use tests::resolved_provider_auth_from_credentials;
