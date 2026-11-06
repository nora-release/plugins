use nora_ai::AwsSigV4Credentials;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use nora_provider_subscription_core as core;
use serde_json::{json, Value};

pub(crate) struct Account {
    pub key: String,
    pub region: String,
    pub aws: Option<AwsSigV4Credentials>,
}
impl Account {
    pub fn saved(value: &Value, manifest: &Value, mantle: bool) -> PluginResult<Self> {
        let key = value
            .get("access")
            .or_else(|| value.get("accessToken"))
            .or_else(|| value.get("key"))
            .and_then(core::text)
            .filter(|key| key.len() <= 64 * 1024 && !key.chars().any(char::is_control))
            .ok_or_else(|| PluginError::authentication("Connect an Amazon Bedrock account."))?
            .to_owned();
        let region = value
            .get("region")
            .or_else(|| value.pointer("/extras/region"))
            .and_then(core::text)
            .or_else(|| {
                manifest
                    .get("baseURL")
                    .and_then(core::text)
                    .filter(|base| !base.contains("://"))
            })
            .unwrap_or("us-east-1")
            .to_owned();
        validate_region(&region)?;
        let aws = parse_aws(&key);
        if aws.as_ref().is_some_and(|aws| {
            [
                Some(aws.access_key_id.as_str()),
                Some(aws.secret_access_key.as_str()),
                aws.session_token.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|part| {
                part.is_empty() || part.len() > 64 * 1024 || part.chars().any(char::is_control)
            })
        }) {
            return Err(PluginError::authentication(
                "The saved AWS credentials are invalid.",
            ));
        }
        if mantle && aws.is_some() {
            return Err(PluginError::invalid(
                "Bedrock Mantle requires a Bedrock API key.",
            ));
        }
        Ok(Self { key, region, aws })
    }
    pub fn root(&self, mantle: bool) -> String {
        if mantle {
            format!("https://bedrock-mantle.{}.api.aws", self.region)
        } else {
            format!("https://bedrock-runtime.{}.amazonaws.com", self.region)
        }
    }
    pub fn from_inputs(params: &Value, manifest: &Value, mantle: bool) -> PluginResult<Value> {
        let inputs = core::inputs(params);
        let method = params
            .get("authMethodID")
            .or_else(|| params.get("method"))
            .and_then(core::text)
            .unwrap_or("apiKey");
        let key = match method {
            "apiKey" => core::required(inputs, "apiKey")
                .or_else(|_| core::required(inputs, "key"))?
                .to_owned(),
            "aws" if !mantle => {
                let access = core::required(inputs, "accessKeyId")?;
                let secret = core::required(inputs, "secretAccessKey")?;
                let token = inputs.get("sessionToken").and_then(core::text);
                if token.is_some_and(|token| {
                    token.chars().any(char::is_control) || token.len() > 64 * 1024
                }) {
                    return Err(PluginError::invalid("The AWS session token is invalid."));
                }
                json!({"accessKeyId":access,"secretAccessKey":secret,"sessionToken":token})
                    .to_string()
            }
            _ => return Err(PluginError::unsupported(method)),
        };
        let value = json!({"type":"api","authMethod":method,"access":key,"region":inputs.get("region").and_then(core::text).unwrap_or("us-east-1"),"expires":0});
        let account = Self::saved(&value, manifest, mantle)?;
        if method == "aws" && account.aws.is_none() {
            return Err(PluginError::invalid("The AWS credentials are invalid."));
        }
        Ok(value)
    }
}
pub(crate) fn validate_region(region: &str) -> PluginResult<()> {
    let parts = region.split('-').collect::<Vec<_>>();
    if region.len() > 64
        || parts.len() < 3
        || !parts
            .last()
            .is_some_and(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
        || !region
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
        || parts.iter().any(|part| part.is_empty())
    {
        return Err(PluginError::invalid(
            "Enter an AWS region such as us-east-1.",
        ));
    }
    Ok(())
}
fn parse_aws(raw: &str) -> Option<AwsSigV4Credentials> {
    if let Ok(value) = serde_json::from_str::<Value>(raw) {
        return Some(AwsSigV4Credentials {
            access_key_id: value
                .get("accessKeyId")
                .or_else(|| value.get("AWS_ACCESS_KEY_ID"))
                .and_then(core::text)?
                .to_owned(),
            secret_access_key: value
                .get("secretAccessKey")
                .or_else(|| value.get("AWS_SECRET_ACCESS_KEY"))
                .and_then(core::text)?
                .to_owned(),
            session_token: value
                .get("sessionToken")
                .or_else(|| value.get("AWS_SESSION_TOKEN"))
                .and_then(core::text)
                .map(str::to_owned),
        });
    }
    let mut parts = raw.splitn(3, ':');
    let access = parts.next()?;
    if !access.starts_with("AKIA") && !access.starts_with("ASIA") {
        return None;
    }
    Some(AwsSigV4Credentials {
        access_key_id: access.to_owned(),
        secret_access_key: parts.next().filter(|part| !part.is_empty())?.to_owned(),
        session_token: parts
            .next()
            .filter(|part| !part.is_empty())
            .map(str::to_owned),
    })
}

#[cfg(test)]
#[path = "../test/config.test.rs"]
mod tests;
