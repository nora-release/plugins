use nora_ai::{HttpClient, JsonValue, OAuthCredentials, ResolvedProviderAuth};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use nora_provider_quota::{fetch, fetch_between, unsupported_origin};
use serde_json::Value;

pub fn configure(inputs: &Value, credentials: &mut OAuthCredentials) -> PluginResult<()> {
    let mut values = Vec::new();
    for (input, key) in [
        ("organizationID", "glmOrganizationID"),
        ("projectID", "glmProjectID"),
    ] {
        if let Some(value) = inputs
            .get(input)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if value.len() > 512 || value.chars().any(char::is_control) {
                return Err(PluginError::invalid(
                    "The GLM team organization or project is invalid.",
                ));
            }
            values.push((key, value));
        }
    }
    if values.len() == 1 {
        return Err(PluginError::invalid(
            "Enter both the team's organization and project IDs.",
        ));
    }
    for (key, value) in values {
        credentials
            .extras
            .insert(key.into(), JsonValue::string(value));
    }
    Ok(())
}

pub fn enrich(credentials: &OAuthCredentials, auth: &mut ResolvedProviderAuth) {
    for key in ["glmOrganizationID", "glmProjectID"] {
        if let Some(value) = credentials.extras.get(key) {
            auth.metadata.insert(key.into(), value.clone());
        }
    }
}

pub async fn usage(
    auth: &ResolvedProviderAuth,
    host: &str,
    endpoint: &str,
    client: &dyn HttpClient,
) -> Result<Value, String> {
    let organization = auth
        .metadata
        .get("glmOrganizationID")
        .and_then(|value| value.as_str());
    let project = auth
        .metadata
        .get("glmProjectID")
        .and_then(|value| value.as_str());
    let (Some(organization), Some(project)) = (organization, project) else {
        let Some(payload) = fetch(auth, host, endpoint, false, client)
            .await
            .map_err(|error| error.to_string())?
        else {
            return Ok(unsupported_origin());
        };
        return super::usage::parse(&payload);
    };
    let headers = vec![
        ("Bigmodel-Organization".into(), organization.into()),
        ("Bigmodel-Project".into(), project.into()),
        (
            "Set-Language".into(),
            if host == "open.bigmodel.cn" {
                "zh"
            } else {
                "en"
            }
            .into(),
        ),
    ];
    let query = format!("{endpoint}?type=2");
    let first = fetch_between(auth, host, host, &query, false, &headers, client).await;
    match first {
        Ok(None) => return Ok(unsupported_origin()),
        Ok(Some(payload)) => {
            if let Ok(mut result) = super::usage::parse(&payload) {
                result["account"]["plan"] = "GLM Coding Team".into();
                return Ok(result);
            }
        }
        Err(nora_provider_quota::QuotaError::Status(401 | 403)) => {
            return Err("The GLM team key was refused.".into())
        }
        _ => {}
    }
    if host == "open.bigmodel.cn" {
        let Some(payload) = fetch_between(
            auth,
            host,
            "bigmodel.cn",
            "https://bigmodel.cn/api/monitor/usage/quota/limit?type=2",
            false,
            &headers,
            client,
        )
        .await
        .map_err(|error| error.to_string())?
        else {
            return Ok(unsupported_origin());
        };
        let mut result = super::usage::parse(&payload)?;
        result["account"]["plan"] = "GLM Coding Team".into();
        return Ok(result);
    }
    Err(
        "The GLM team allowance could not be read. Check the key, organization, and project IDs."
            .into(),
    )
}

#[cfg(test)]
#[path = "../test/team.test.rs"]
mod tests;
