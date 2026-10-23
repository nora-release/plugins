use crate::config::{reconnect, valid_account, Config};
use futures::StreamExt;
use nora_ai::{HttpClient, HttpMethod, HttpRequest};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{fs::File, io::Read};

const MAX_FILE: usize = 1024 * 1024;
const MAX_REPLY: usize = 2 * 1024 * 1024;
pub(crate) const TOKEN: &str = "https://oauth2.googleapis.com/token";
pub(crate) const IAM: &str = "https://iamcredentials.googleapis.com/v1";
pub(crate) const SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";

#[derive(Clone)]
pub(crate) struct Token {
    pub access: String,
    pub expires: i64,
}

pub(crate) struct Source {
    pub value: Value,
    pub identity: String,
    pub stamp: String,
}

pub(crate) fn now() -> i64 {
    chrono::Utc::now().timestamp_millis()
}

pub(crate) fn read(config: &Config) -> PluginResult<Source> {
    let file = File::open(&config.source_path).map_err(|_| reconnect())?;
    let metadata = file.metadata().map_err(|_| reconnect())?;
    if !metadata.is_file() || metadata.len() > MAX_FILE as u64 {
        return Err(reconnect());
    }
    let mut bytes = Vec::new();
    file.take(MAX_FILE as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| reconnect())?;
    if bytes.len() > MAX_FILE {
        return Err(reconnect());
    }
    let value: Value = serde_json::from_slice(&bytes).map_err(|_| reconnect())?;
    let identity = identity(&value, true)?;
    Ok(Source {
        value,
        identity,
        stamp: format!("{:x}", Sha256::digest(&bytes)),
    })
}

fn text<'a>(value: &'a Value, key: &str) -> PluginResult<&'a str> {
    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| {
            !value.trim().is_empty()
                && value.len() <= 64 * 1024
                && !value.chars().any(char::is_control)
        })
        .ok_or_else(reconnect)
}

fn universe(value: &Value) -> PluginResult<()> {
    if value
        .get("universe_domain")
        .and_then(Value::as_str)
        .is_some_and(|domain| !domain.is_empty() && domain != "googleapis.com")
    {
        return Err(PluginError::invalid("These credentials belong to another Google universe. This provider uses googleapis.com."));
    }
    Ok(())
}

fn impersonated(value: &Value) -> PluginResult<String> {
    let url = url::Url::parse(text(value, "service_account_impersonation_url")?)
        .map_err(|_| reconnect())?;
    let raw = url
        .path()
        .rsplit_once("/serviceAccounts/")
        .and_then(|(_, tail)| tail.strip_suffix(":generateAccessToken"))
        .ok_or_else(reconnect)?;
    let account = percent_encoding::percent_decode_str(raw)
        .decode_utf8()
        .map_err(|_| reconnect())?
        .into_owned();
    if !valid_account(&account) {
        return Err(reconnect());
    }
    Ok(account)
}

fn identity(value: &Value, top: bool) -> PluginResult<String> {
    universe(value)?;
    let identity = match text(value, "type")? {
        "authorized_user" => format!(
            "person\0{}\0{}",
            text(value, "client_id")?,
            text(value, "refresh_token")?
        ),
        "service_account" => {
            let email = text(value, "client_email")?;
            if !valid_account(email) {
                return Err(reconnect());
            }
            format!("service\0{email}")
        }
        "impersonated_service_account" if top => format!(
            "impersonated\0{}\0{}",
            impersonated(value)?,
            identity(&value["source_credentials"], false)?
        ),
        "external_account" | "external_account_authorized_user" => {
            return Err(PluginError::unsupported("workload identity federation"))
        }
        _ => return Err(reconnect()),
    };
    Ok(format!("{:x}", Sha256::digest(identity.as_bytes())))
}

pub(crate) async fn json_request(
    client: &dyn HttpClient,
    request: HttpRequest,
) -> PluginResult<Value> {
    let response = client
        .stream(request)
        .await
        .map_err(|_| PluginError::transport("Google could not be reached."))?;
    if !(200..300).contains(&response.status) {
        let message = format!(
            "Google returned HTTP {}. Reconnect revoked credentials or retry a temporary failure.",
            response.status
        );
        return Err(if matches!(response.status, 401 | 403) {
            PluginError::authentication(message)
        } else {
            PluginError::transport(message)
        });
    }
    let mut bytes = Vec::new();
    let mut body = response.body;
    while let Some(chunk) = body.next().await {
        let chunk =
            chunk.map_err(|_| PluginError::transport("The Google response was interrupted."))?;
        if bytes.len().saturating_add(chunk.len()) > MAX_REPLY {
            return Err(PluginError::transport("The Google response is too large."));
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes)
        .map_err(|_| PluginError::transport("Google returned invalid JSON."))
}

fn access_token(value: &Value, name: &str) -> PluginResult<String> {
    let token = text(value, name)?;
    if token.chars().any(char::is_whitespace) {
        return Err(reconnect());
    }
    Ok(token.into())
}

async fn grant(client: &dyn HttpClient, fields: &[(&str, String)]) -> PluginResult<Token> {
    let form = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(fields.iter().map(|(key, value)| (*key, value.as_str())))
        .finish();
    let value = json_request(
        client,
        HttpRequest::new(HttpMethod::Post, TOKEN)
            .with_header("content-type", "application/x-www-form-urlencoded")
            .with_body(form.into_bytes()),
    )
    .await?;
    let seconds = match value.get("expires_in") {
        None => 3600,
        Some(value) => value
            .as_i64()
            .filter(|value| *value > 0 && *value <= 86400)
            .ok_or_else(reconnect)?,
    };
    Ok(Token {
        access: access_token(&value, "access_token")?,
        expires: now() + seconds * 1000,
    })
}

async fn impersonate(
    client: &dyn HttpClient,
    token: &Token,
    account: &str,
    delegates: &[Value],
) -> PluginResult<Token> {
    if !valid_account(account) || delegates.len() > 16 {
        return Err(reconnect());
    }
    let mut chain = Vec::new();
    for delegate in delegates {
        let raw = delegate.as_str().ok_or_else(reconnect)?;
        let value = if let Some((prefix, email)) = raw.rsplit_once("/serviceAccounts/") {
            if !prefix.starts_with("projects/")
                || prefix[9..].contains('/')
                || !valid_account(email)
            {
                return Err(reconnect());
            }
            raw.to_string()
        } else {
            if !valid_account(raw) {
                return Err(reconnect());
            }
            format!("projects/-/serviceAccounts/{raw}")
        };
        chain.push(value);
    }
    let mut body = json!({"scope":[SCOPE],"lifetime":"3600s"});
    if !chain.is_empty() {
        body["delegates"] = json!(chain);
    }
    let account =
        percent_encoding::utf8_percent_encode(account, percent_encoding::NON_ALPHANUMERIC);
    let request = HttpRequest::new(
        HttpMethod::Post,
        format!("{IAM}/projects/-/serviceAccounts/{account}:generateAccessToken"),
    )
    .with_header("authorization", format!("Bearer {}", token.access))
    .with_json_body(&body)
    .map_err(|_| reconnect())?;
    let value = json_request(client, request).await?;
    let expires = match value.get("expireTime") {
        None => now() + 3_600_000,
        Some(value) => chrono::DateTime::parse_from_rfc3339(value.as_str().ok_or_else(reconnect)?)
            .map_err(|_| reconnect())?
            .timestamp_millis(),
    };
    if expires <= now() {
        return Err(reconnect());
    }
    Ok(Token {
        access: access_token(&value, "accessToken")?,
        expires,
    })
}

async fn mint_one(client: &dyn HttpClient, value: &Value, top: bool) -> PluginResult<Token> {
    universe(value)?;
    match text(value, "type")? {
        "authorized_user" => {
            grant(
                client,
                &[
                    ("grant_type", "refresh_token".into()),
                    ("client_id", text(value, "client_id")?.into()),
                    (
                        "client_secret",
                        value
                            .get("client_secret")
                            .and_then(Value::as_str)
                            .unwrap_or("")
                            .into(),
                    ),
                    ("refresh_token", text(value, "refresh_token")?.into()),
                ],
            )
            .await
        }
        "service_account" => {
            let email = text(value, "client_email")?;
            if !valid_account(email) {
                return Err(reconnect());
            }
            // Parse the PEM directly: source credentials and private keys never enter Nora's account store.
            let key = value
                .get("private_key")
                .and_then(Value::as_str)
                .ok_or_else(reconnect)?;
            let key =
                jsonwebtoken::EncodingKey::from_rsa_pem(key.as_bytes()).map_err(|_| reconnect())?;
            let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
            header.kid = value
                .get("private_key_id")
                .and_then(Value::as_str)
                .filter(|key| key.len() <= 512)
                .map(str::to_owned);
            let now = now() / 1000 - 10;
            let assertion = jsonwebtoken::encode(
                &header,
                &json!({"iss":email,"scope":SCOPE,"aud":TOKEN,"iat":now,"exp":now+3600}),
                &key,
            )
            .map_err(|_| reconnect())?;
            grant(
                client,
                &[
                    (
                        "grant_type",
                        "urn:ietf:params:oauth:grant-type:jwt-bearer".into(),
                    ),
                    ("assertion", assertion),
                ],
            )
            .await
        }
        "impersonated_service_account" if top => {
            let account = impersonated(value)?;
            let source = Box::pin(mint_one(client, &value["source_credentials"], false)).await?;
            let delegates = value
                .get("delegates")
                .and_then(Value::as_array)
                .map(Vec::as_slice)
                .unwrap_or(&[]);
            impersonate(client, &source, &account, delegates).await
        }
        _ => Err(reconnect()),
    }
}

pub(crate) async fn mint(
    client: &dyn HttpClient,
    source: &Source,
    config: &Config,
) -> PluginResult<Token> {
    tokio::time::timeout(std::time::Duration::from_secs(30), async {
        let token = mint_one(client, &source.value, true).await?;
        if config.impersonate.is_empty() {
            return Ok(token);
        }
        impersonate(client, &token, &config.impersonate, &[]).await
    })
    .await
    .map_err(|_| PluginError::transport("The Google credential exchange timed out."))?
}

#[cfg(test)]
#[path = "../test/credentials.test.rs"]
mod tests;
