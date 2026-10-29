use super::*;
use nora_ai::HttpClient;
use nora_provider::account_import::{self as shared, CredentialPersistence, ImportInput};
use std::collections::BTreeMap;

mod parse;

pub(crate) const FORMATS: &[&str] = &[
    "auto",
    "google-account",
    "antigravity-manager",
    "cli-proxy",
    "refresh-token",
    "nora-account",
    "magpie-login",
];

pub(crate) fn inspect(app: App, params: &Value) -> PluginResult<Value> {
    nora_provider_plugin_sdk::require_subscription_accounts()?;
    let input = ImportInput::read(params)?;
    let entries = parse::parse(app, &input)?;
    let secrets = entries
        .iter()
        .flat_map(parse::Entry::secrets)
        .collect::<Vec<_>>();
    let mut fingerprints = BTreeMap::new();
    let mut emails = BTreeMap::new();
    let mut public = Vec::new();
    for (index, entry) in entries.iter().enumerate() {
        let label = if entry.email.is_empty() {
            format!("#{}", index + 1)
        } else {
            entry.email.clone()
        };
        let mut item = json!({"index":index,"label":shared::redact(label,&secrets)});
        if let Some(error) = &entry.error {
            item["error"] = json!(error);
        } else {
            let fingerprint = shared::fingerprint(app.id(), &entry.refresh);
            let previous = fingerprints.get(&fingerprint).or_else(|| {
                if entry.email.is_empty() {
                    None
                } else {
                    emails.get(&entry.email.to_lowercase())
                }
            });
            if let Some(previous) = previous {
                item["duplicateOf"] = json!(previous);
            } else {
                fingerprints.insert(fingerprint.clone(), index);
                if !entry.email.is_empty() {
                    emails.insert(entry.email.to_lowercase(), index);
                }
            }
            item["fingerprint"] = json!(fingerprint);
        }
        public.push(item);
    }
    Ok(json!({"entries":public}))
}

pub(super) async fn import(
    plugin: &CodeAssist,
    params: &Value,
    client: &dyn HttpClient,
    persistence: &dyn CredentialPersistence,
) -> PluginResult<Value> {
    nora_provider_plugin_sdk::require_subscription_accounts()?;
    let input = ImportInput::read(params)?;
    let entries = parse::parse(plugin.app, &input)?;
    let index = params
        .get("index")
        .map(|value| {
            value
                .as_u64()
                .and_then(|index| usize::try_from(index).ok())
                .ok_or_else(|| shared::invalid("Choose a valid account entry."))
        })
        .transpose()?
        .unwrap_or(0);
    let entry = entries
        .get(index)
        .ok_or_else(|| shared::invalid("Choose a valid account entry."))?;
    if let Some(error) = &entry.error {
        return Err(shared::invalid(error));
    }
    let fingerprint = shared::fingerprint(plugin.app.id(), &entry.refresh);
    if params
        .get("fingerprint")
        .is_some_and(|value| value.as_str() != Some(&fingerprint))
    {
        return Err(shared::invalid(
            "The selected import entry changed. Inspect the file again.",
        ));
    }
    let mut credentials = match shared::resumed(params, &fingerprint)? {
        Some(credentials) => {
            if credentials["type"] != "oauth"
                || credentials["app"] != plugin.app.id()
                || token(credentials.get("refresh")).is_err()
            {
                return Err(shared::invalid(
                    "The saved import does not match this Google application.",
                ));
            }
            credentials
        }
        None => {
            let tokens = session::Tokens {
                refresh: entry.refresh.clone(),
                access: entry.access.clone(),
                project: entry.project.clone(),
                email: entry.email.clone(),
                ..Default::default()
            };
            let mut credentials = session::saved(&tokens, plugin.app, None);
            shared::mark(&mut credentials, &fingerprint, "pending");
            credentials
        }
    };
    shared::checkpoint(persistence, &credentials).await?;
    let valid = token(credentials.get("access")).is_ok()
        && credentials
            .get("expires")
            .and_then(Value::as_i64)
            .is_some_and(|expiry| expiry > now().saturating_add(30_000));
    if !valid {
        let refresh = token(credentials.get("refresh"))?;
        let payload = auth::exchange(
            plugin.app,
            &[("grant_type", "refresh_token"), ("refresh_token", &refresh)],
            client,
        )
        .await?;
        let rotated = payload
            .get("refresh_token")
            .map(|value| token(Some(value)))
            .transpose();
        if let Ok(Some(refresh)) = &rotated {
            credentials["refresh"] = json!(refresh);
        }
        let access = token(payload.get("access_token"));
        if let Ok(access) = &access {
            credentials["access"] = json!(access);
        }
        let seconds = payload
            .get("expires_in")
            .and_then(Value::as_i64)
            .filter(|seconds| *seconds > 0 && *seconds <= 31_536_000);
        credentials["expires"] = json!(seconds
            .map(|seconds| now().saturating_add(seconds.saturating_mul(1000)))
            .unwrap_or(0));
        shared::mark(&mut credentials, &fingerprint, "rotated");
        shared::checkpoint(persistence, &credentials).await?;
        if access.is_err() || rotated.is_err() || seconds.is_none() {
            return Err(auth::invalid_token_response());
        }
    }
    let (mut tokens, _) = session::current(Some(&credentials), plugin.app)?;
    let identity = match auth::identify_with_identity(&mut tokens, client).await {
        Ok(identity) => identity,
        Err(error) => {
            if entry.email.is_empty() {
                return Err(error);
            }
            tokens.email = entry.email.clone();
            tokens.uid =
                super::fingerprint(&format!("google-account\0{}", tokens.email.to_lowercase()));
            json!({"issuer":"urn:nora:import:google-unverified","subject":tokens.email.to_lowercase()})
        }
    };
    let identity = if identity["issuer"] == "https://accounts.google.com" {
        identity
    } else {
        json!({"issuer":"urn:nora:import:google-unverified","subject":tokens.email.to_lowercase()})
    };
    credentials = session::saved(&tokens, plugin.app, None);
    credentials["accountIdentity"] = identity.clone();
    shared::mark(&mut credentials, &fingerprint, "identified");
    shared::checkpoint(persistence, &credentials).await?;
    tokens.version = plugin.client_version(client).await;
    let project = plugin.project(&tokens, client).await.ok();
    if let Some(project) = &project {
        tokens.project = project.id.clone();
    }
    let mut credentials = session::saved(&tokens, plugin.app, None);
    let mut secrets = entry.secrets();
    secrets.extend([tokens.access.clone(), tokens.refresh.clone()]);
    for key in ["email", "name"] {
        if let Some(value) = credentials[key].as_str() {
            credentials[key] = json!(shared::redact(value.to_string(), &secrets));
        }
    }
    credentials["label"] = credentials["email"].clone();
    if let Some(project) = project {
        credentials["plan"] = json!(shared::redact(project.plan, &secrets));
    }
    credentials["accountIdentity"] = identity;
    shared::mark(&mut credentials, &fingerprint, "complete");
    shared::checkpoint(persistence, &credentials).await?;
    shared::result(credentials, true)
}

#[cfg(test)]
#[path = "../test/account_import.test.rs"]
mod tests;
