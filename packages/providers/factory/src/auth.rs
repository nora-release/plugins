use super::{
    account::{self, Account},
    network,
};
use nora_ai::HttpClient;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use nora_provider_subscription_core as core;
use serde_json::{json, Value};
pub(crate) async fn token(
    client: &dyn HttpClient,
    fields: &[(&str, &str)],
) -> PluginResult<(u16, Value)> {
    network::read(
        network::send(
            client,
            core::form_request(format!("{}/authenticate", network::WORKOS), fields),
        )
        .await?,
    )
    .await
}
pub(crate) async fn renew(
    client: &dyn HttpClient,
    old: &Account,
    org: &str,
) -> PluginResult<Account> {
    if old.key || old.refresh.is_empty() {
        return Err(account::expired());
    }
    let mut fields = vec![
        ("grant_type", "refresh_token"),
        ("refresh_token", old.refresh.as_str()),
        ("client_id", network::CLIENT_ID),
    ];
    if !org.is_empty() {
        fields.push(("organization_id", org));
    }
    let (status, value) = token(client, &fields).await?;
    if status != 200 || value.get("error").is_some() {
        return Err(network::error(status.max(400)));
    }
    let next = account::from_token(&value)?;
    let mut result = old.clone();
    result.access = next.access;
    result.expires = next.expires;
    if !next.refresh.is_empty() {
        result.refresh = next.refresh;
    }
    if !next.org.is_empty() {
        result.org = next.org;
    } else if !org.is_empty() {
        result.org = org.into();
    }
    if !next.user.is_empty() && !old.user.is_empty() && next.user != old.user {
        return Err(account::expired());
    }
    if result.user.is_empty() {
        result.user = next.user;
    }
    if result.email.is_empty() {
        result.email = next.email;
    }
    if let Err(error) = network::reconcile(client, &mut result).await {
        if error.code == "authentication_required" || error.code == "invalid_request" {
            return Err(error);
        }
        // WorkOS already rotated the refresh token. Keep the new pair when
        // Factory's profile endpoint is temporarily unavailable.
    }
    Ok(result)
}
pub(crate) async fn first_org(client: &dyn HttpClient, account: &Account) -> PluginResult<String> {
    let mut unscoped = account.clone();
    unscoped.active_org.clear();
    let value = network::checked(
        client,
        network::factory(
            nora_ai::HttpMethod::Get,
            format!("{}/api/cli/org", account.base()),
            &unscoped,
        ),
    )
    .await?;
    Ok(account::text(value.pointer("/workosOrgIds/0"), 256)
        .unwrap_or_default()
        .into())
}
pub(crate) async fn signed(client: &dyn HttpClient, value: &Value) -> PluginResult<Account> {
    let mut account = account::from_token(value)?;
    if account.org.is_empty()
        && core::text(&core::decode_jwt(&account.access)["external_org_id"]).is_none()
    {
        let org = first_org(client, &account).await.unwrap_or_default();
        if !org.is_empty() {
            account = renew(client, &account, &org).await?;
        }
    }
    if let Err(error) = network::reconcile(client, &mut account).await {
        if account.identity().is_empty()
            || matches!(
                error.code.as_str(),
                "invalid_request" | "authentication_required"
            )
        {
            return Err(error);
        }
    }
    if account.identity().is_empty() {
        return Err(PluginError::authentication(
            "Factory did not identify this account. Sign in again.",
        ));
    }
    Ok(account)
}
pub(crate) async fn start(client: &dyn HttpClient) -> PluginResult<(Value, Value)> {
    let value = network::checked(
        client,
        core::form_request(
            format!("{}/authorize/device", network::WORKOS),
            &[("client_id", network::CLIENT_ID)],
        ),
    )
    .await?;
    let device = core::required(&value, "device_code")?;
    let code = core::required(&value, "user_code")?;
    let url = core::text(&value["verification_uri_complete"])
        .or_else(|| core::text(&value["verification_uri"]))
        .ok_or_else(|| PluginError::transport("Factory returned no verification URL."))?;
    let parsed = url::Url::parse(url)
        .map_err(|_| PluginError::transport("Factory returned an invalid verification URL."))?;
    if parsed.scheme() != "https" || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(PluginError::transport(
            "Factory returned an unsafe verification URL.",
        ));
    }
    let interval = value["interval"].as_u64().unwrap_or(1).clamp(1, 60) * 1000;
    let until = core::now()
        .saturating_add(value["expires_in"].as_u64().unwrap_or(300).clamp(300, 900) * 1000 + 30000);
    Ok((
        json!({"device":device,"interval":interval,"until":until}),
        json!({"url":url,"method":"auto","instructions":format!("Confirm the code {code} on Factory's page.")}),
    ))
}
pub(crate) async fn finish(client: &dyn HttpClient, mut pending: Value) -> PluginResult<Account> {
    loop {
        let interval = pending["interval"]
            .as_u64()
            .unwrap_or(1000)
            .clamp(1000, 60000);
        tokio::time::sleep(std::time::Duration::from_millis(interval)).await;
        if core::now() >= pending["until"].as_u64().unwrap_or(0) {
            return Err(account::expired());
        }
        let device = core::required(&pending, "device")?;
        let result = token(
            client,
            &[
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
                ("device_code", device),
                ("client_id", network::CLIENT_ID),
            ],
        )
        .await;
        let (status, value) = match result {
            Ok(result) => result,
            Err(error) if error.code == "upstream_transport_error" => continue,
            Err(error) => return Err(error),
        };
        match value["error"].as_str() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                pending["interval"] = json!((interval + 1000).min(60000));
                continue;
            }
            Some("expired_token" | "access_denied") => return Err(account::expired()),
            Some(_) => return Err(network::error(status.max(400))),
            None if status == 200 => return signed(client, &value).await,
            None => return Err(network::error(status)),
        }
    }
}
#[cfg(test)]
#[path = "../test/auth.test.rs"]
mod tests;
