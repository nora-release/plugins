use crate::{
    endpoint::{self, Site},
    readings,
};
use aes_gcm::{
    aead::{Aead, KeyInit},
    Aes256Gcm, Nonce,
};
use base64::Engine;
use nora_ai::{HttpClient, HttpMethod};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use nora_provider_subscription_core as core;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};
pub(crate) fn key_url(site: Site, org: &str, project: &str) -> String {
    format!(
        "{}/api/biz/v1/organization/{}/projects/{}/api_keys",
        site.api(),
        core::encode(org),
        core::encode(project)
    )
}
pub(crate) async fn project_key(
    client: &dyn HttpClient,
    site: Site,
    auth: &str,
    org: &str,
    project: &str,
    team: bool,
    create: bool,
    device: &str,
) -> PluginResult<String> {
    let url = key_url(site, org, project);
    let headers = if team {
        endpoint::team_headers(site, org, project)
    } else {
        BTreeMap::new()
    };
    let list = endpoint::call(
        client,
        HttpMethod::Get,
        &url,
        Some(auth),
        None,
        headers.clone(),
        device,
    )
    .await?;
    let name = if team {
        "zcode-team-api-key"
    } else {
        "zcode-api-key"
    };
    let mut id = list
        .as_array()
        .into_iter()
        .flatten()
        .filter(|entry| {
            entry["name"] == name && (!team || entry["keyType"] == 2 || entry["keyType"] == "2")
        })
        .filter_map(|entry| core::text(&entry["apiKey"]))
        .last()
        .map(str::to_owned);
    if id.is_none() {
        if !create {
            return Err(PluginError::authentication("The selected ZCode source has no existing plan key. Create it in ZCode before reconnecting."));
        }
        let mut body = json!({"name":name});
        if team {
            body["keyType"] = json!(2);
        }
        let key = endpoint::call(
            client,
            HttpMethod::Post,
            &url,
            Some(auth),
            Some(&body),
            headers.clone(),
            device,
        )
        .await?;
        id = core::text(&key["apiKey"]).map(str::to_owned);
    }
    let id = id.ok_or_else(|| PluginError::authentication("ZCode returned no plan key ID."))?;
    let copy = endpoint::call(
        client,
        HttpMethod::Get,
        &format!("{url}/copy/{}", core::encode(&id)),
        Some(auth),
        None,
        headers,
        device,
    )
    .await?;
    if let Some(secret) = core::text(&copy["secretKey"]) {
        return Ok(format!("{id}.{secret}"));
    }
    if team {
        return Ok(id);
    }
    Err(PluginError::authentication(
        "ZCode returned no plan key secret.",
    ))
}
async fn team_signin(
    client: &dyn HttpClient,
    site: Site,
    business: &str,
    info: &Value,
    jwt: &str,
    device: &str,
    create: bool,
) -> PluginResult<Option<Value>> {
    let mut unassigned = false;
    for org in info["organizations"].as_array().into_iter().flatten() {
        let Some(org_id) = core::text(&org["organizationId"]) else {
            continue;
        };
        for project in org["projects"]
            .as_array()
            .into_iter()
            .flatten()
            .filter(|project| project["projectType"] == 2 || project["projectType"] == "2")
        {
            let Some(project_id) = core::text(&project["projectId"]) else {
                continue;
            };
            let headers = endpoint::team_headers(site, org_id, project_id);
            let detail = match endpoint::call(
                client,
                HttpMethod::Get,
                &format!(
                    "{}/api/biz/team/subscribe/product/querySubscribeDetail",
                    site.api()
                ),
                Some(business),
                None,
                headers,
                device,
            )
            .await
            {
                Ok(detail) => detail,
                Err(_) => continue,
            };
            let effective = detail["status"]
                .as_str()
                .is_some_and(|s| s.eq_ignore_ascii_case("EFFECTIVE"));
            if effective
                && detail["memberGrantStatus"]
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case("UNASSIGNED"))
            {
                unassigned = true;
            }
            if detail["hasSubscription"] == false
                || !effective
                || !detail["memberGrantStatus"]
                    .as_str()
                    .is_some_and(|s| s.eq_ignore_ascii_case("VALID"))
            {
                continue;
            }
            let key = project_key(
                client, site, business, org_id, project_id, true, create, device,
            )
            .await?;
            return Ok(Some(
                json!({"site":site.id(),"base":site.base(),"key":key,"token":business,"org":org_id,"project":project_id,"jwt":jwt,"device":device,"plan":core::text(&detail["productName"]).unwrap_or("GLM Coding Team")}),
            ));
        }
    }
    if unassigned {
        return Err(PluginError::authentication("This ZCode team account has no assigned Coding Plan seat. Ask the team administrator to assign one."));
    }
    Ok(None)
}
pub(crate) async fn sign_in(
    client: &dyn HttpClient,
    site: Site,
    token: &str,
    jwt: &str,
    device: &str,
    team: bool,
) -> PluginResult<Value> {
    let business: PluginResult<String> = async {
        if site == Site::BigModel {
            return Ok(token.to_owned());
        }
        let value = endpoint::call(
            client,
            HttpMethod::Post,
            &format!("{}/api/auth/z/login", site.api()),
            None,
            Some(&json!({"token":token})),
            BTreeMap::new(),
            device,
        )
        .await?;
        Ok(format!(
            "Bearer {}",
            core::required(&value, "access_token")?
        ))
    }
    .await;
    let (business, info) = match business {
        Ok(business) => {
            let info = endpoint::call(
                client,
                HttpMethod::Get,
                &format!("{}/api/biz/customer/getCustomerInfo", site.api()),
                Some(&business),
                None,
                BTreeMap::new(),
                device,
            )
            .await;
            (business, info)
        }
        Err(error) => (
            String::new(),
            Err(endpoint::Error {
                status: 0,
                message: error.message,
            }),
        ),
    };
    let mut own = None;
    let mut failure = None;
    if let Ok(info) = &info {
        if !team {
            let mut chosen = None;
            for org in info["organizations"].as_array().into_iter().flatten() {
                let Some(org_id) = core::text(&org["organizationId"]) else {
                    continue;
                };
                let projects = org["projects"]
                    .as_array()
                    .into_iter()
                    .flatten()
                    .filter(|p| {
                        p["projectType"] != 2
                            && p["projectType"] != "2"
                            && core::text(&p["projectId"]).is_some()
                    })
                    .collect::<Vec<_>>();
                if projects.is_empty() {
                    continue;
                }
                let project = projects
                    .iter()
                    .find(|p| {
                        p["projectName"]
                            .as_str()
                            .is_some_and(|name| name.contains("默认项目"))
                    })
                    .unwrap_or(&projects[0]);
                let named = org["organizationName"]
                    .as_str()
                    .is_some_and(|name| name.contains("默认机构"));
                if chosen.is_none() || named {
                    chosen = Some((org_id, core::required(project, "projectId")?));
                    if named {
                        break;
                    }
                }
            }
            if let Some((org, project)) = chosen {
                match project_key(client, site, &business, org, project, false, true, device).await
                {
                    Ok(key) => {
                        let state = json!({"site":site.id(),"base":site.base(),"key":key,"jwt":jwt,"device":device});
                        match endpoint::call(
                            client,
                            HttpMethod::Get,
                            &format!("{}/api/biz/subscription/list", site.root()),
                            Some(core::required(&state, "key")?),
                            None,
                            BTreeMap::new(),
                            device,
                        )
                        .await
                        {
                            Ok(subs) => {
                                if let Some(plan) = readings::plan(&subs) {
                                    let mut state = state;
                                    state["plan"] = json!(plan);
                                    return Ok(state);
                                }
                                own = Some(state);
                            }
                            Err(error) => failure = Some(PluginError::from(error)),
                        }
                    }
                    Err(error) => failure = Some(error),
                }
            }
        }
        match team_signin(client, site, &business, info, jwt, device, true).await {
            Ok(Some(state)) => return Ok(state),
            Err(error) if team => return Err(error),
            Err(error) => failure = Some(error),
            _ => {}
        }
    } else {
        failure = Some(PluginError::from(info.unwrap_err()));
    }
    if team {
        return Err(failure.unwrap_or_else(|| {
            PluginError::authentication("This account has no active ZCode team Coding Plan seat.")
        }));
    }
    if !jwt.is_empty() {
        endpoint::live_jwt(jwt)?;
        let balance = endpoint::call(
            client,
            HttpMethod::Get,
            &format!(
                "{}/api/v1/zcode-plan/billing/balance?app_version={}",
                endpoint::ZCODE,
                endpoint::VERSION
            ),
            Some(&format!("Bearer {jwt}")),
            None,
            BTreeMap::new(),
            device,
        )
        .await?;
        if let Some(gift) = readings::gift(&balance, true) {
            let mut state = own.unwrap_or_else(
                || json!({"site":site.id(),"base":site.base(),"key":"","jwt":jwt,"device":device}),
            );
            state["plan"] = json!(gift.plan);
            return Ok(state);
        }
    }
    Err(failure.unwrap_or_else(|| {
        PluginError::authentication(
            "This account has no active Coding Plan, team seat or ZCode Start Plan.",
        )
    }))
}
pub(crate) fn read_json(path: &str) -> PluginResult<(PathBuf, Value)> {
    let path = if let Some(rest) = path.strip_prefix("~/") {
        std::env::var_os("HOME")
            .map(PathBuf::from)
            .ok_or_else(|| PluginError::invalid("Select an absolute ZCode file."))?
            .join(rest)
    } else {
        Path::new(path).to_owned()
    };
    if !path.is_absolute() {
        return Err(PluginError::invalid("Select an absolute ZCode file."));
    }
    let mut options = std::fs::OpenOptions::new();
    options.read(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
    }
    let mut file = options.open(&path).map_err(|_| {
        PluginError::authentication("The selected ZCode source file could not be read.")
    })?;
    let metadata = file.metadata().map_err(|_| {
        PluginError::authentication("The selected ZCode source file could not be read.")
    })?;
    if !metadata.is_file() || metadata.len() == 0 || metadata.len() > 2 * 1024 * 1024 {
        return Err(PluginError::invalid(
            "Select a regular ZCode JSON file smaller than 2 MiB.",
        ));
    }
    let mut bytes = Vec::new();
    file.by_ref()
        .take(2 * 1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| PluginError::authentication("The selected ZCode file could not be read."))?;
    if bytes.len() > 2 * 1024 * 1024 {
        return Err(PluginError::invalid(
            "The selected ZCode file grew beyond its size limit.",
        ));
    }
    let value = serde_json::from_slice(&bytes)
        .map_err(|_| PluginError::invalid("The selected ZCode source file is invalid JSON."))?;
    Ok((path, value))
}
fn secret(custom: Option<&str>) -> [u8; 32] {
    let seed = custom
        .map(str::to_owned)
        .or_else(|| {
            std::env::var("ZCODE_CREDENTIAL_SECRET")
                .ok()
                .filter(|value| !value.is_empty())
        })
        .unwrap_or_else(|| {
            let user = whoami::username();
            let user = user.rsplit('\\').next().unwrap_or(&user);
            format!(
                "zcode-credential-fallback:{}:{}:{user}",
                if cfg!(target_os = "macos") {
                    "darwin"
                } else if cfg!(target_os = "windows") {
                    "win32"
                } else {
                    "linux"
                },
                std::env::var("HOME").unwrap_or_default()
            )
        });
    Sha256::digest(seed.as_bytes()).into()
}
pub(crate) fn decrypt(key: &[u8; 32], value: &Value) -> Option<String> {
    let parts = value
        .as_str()?
        .strip_prefix("enc:v1:")?
        .split('.')
        .collect::<Vec<_>>();
    if parts.len() != 3 {
        return None;
    }
    let decode = |s: &str| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(s.trim_end_matches('='))
            .ok()
    };
    let iv = decode(parts[0])?;
    let tag = decode(parts[1])?;
    if iv.len() != 12 || tag.len() != 16 {
        return None;
    }
    let mut body = decode(parts[2])?;
    body.extend_from_slice(&tag);
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    String::from_utf8(
        cipher
            .decrypt(Nonce::from_slice(&iv), body.as_slice())
            .ok()?,
    )
    .ok()
}
pub(crate) fn source(inputs: &Value, device: &str) -> PluginResult<Value> {
    let (_, store) = read_json(core::required(inputs, "sourcePath")?)?;
    let key = secret(core::text(&inputs["credentialSecret"]));
    let settings = core::text(&inputs["settingsPath"])
        .map(read_json)
        .transpose()?
        .map(|(_, value)| value)
        .unwrap_or(Value::Null);
    let mut site = Site::Zai;
    let mut api_key = String::new();
    if let Some(store) = store.as_object() {
        for (name, value) in store {
            if name.contains(":coding-plan:") && name.ends_with(":api-key") {
                if let Some(value) = decrypt(&key, value)
                    .filter(|value| value.contains('.') && !value.contains(['\r', '\n']))
                {
                    let current = if name.contains(":bigmodel-") {
                        Site::BigModel
                    } else {
                        Site::Zai
                    };
                    if api_key.is_empty() || current == Site::Zai {
                        site = current;
                        api_key = value;
                    }
                }
            }
        }
    }
    if let Some(path) = core::text(&inputs["configPath"]) {
        let mut found_config = false;
        let (_, config) = read_json(path)?;
        for (current, ids) in [
            (
                Site::Zai,
                [
                    "builtin:zai-coding-plan",
                    "account:zai-individual-coding-plan",
                ],
            ),
            (
                Site::BigModel,
                [
                    "builtin:bigmodel-coding-plan",
                    "account:bigmodel-individual-coding-plan",
                ],
            ),
        ] {
            for id in ids {
                let raw = &config["provider"][id]["options"]["apiKey"];
                let candidate = raw
                    .as_str()
                    .filter(|key| !key.starts_with("enc:"))
                    .map(str::to_owned)
                    .or_else(|| decrypt(&key, raw));
                if let Some(candidate) = candidate.filter(|value| {
                    value.contains('.')
                        && !value.chars().any(char::is_whitespace)
                        && !value.contains(['{', '$'])
                }) {
                    site = current;
                    api_key = candidate;
                    found_config = true;
                    break;
                }
            }
            if found_config {
                break;
            }
        }
    }
    let jwt = decrypt(&key, &store["zcodejwttoken"])
        .unwrap_or_default()
        .trim_start_matches("Bearer ")
        .to_owned();
    let families = match settings["providerFamilyDomain"].as_str() {
        Some("zai") => vec![Site::Zai],
        Some("bigmodel") => vec![Site::BigModel],
        _ => vec![Site::Zai, Site::BigModel],
    };
    let mut team = None;
    for family in families {
        let selection = &settings["providerFamilyConnectionSelections"][family.id()];
        let mut org = String::new();
        let mut project = String::new();
        if selection.is_object() {
            if selection["kind"] == "team-coding-plan" {
                org = core::text(&selection["organizationId"])
                    .unwrap_or("")
                    .into();
                project = core::text(&selection["projectId"]).unwrap_or("").into();
            }
        } else if let Some(legacy) =
            core::text(&settings["modelProviderFamilySelectedKeys"][family.id()]).and_then(|key| {
                key.strip_prefix(&format!("team-plan:builtin:{}-coding-plan:", family.id()))
            })
        {
            let parts = legacy.split(':').collect::<Vec<_>>();
            if parts.len() == 3 {
                org = percent_encoding::percent_decode_str(parts[1])
                    .decode_utf8()
                    .map_err(|_| PluginError::invalid("The selected ZCode team ID is invalid."))?
                    .into_owned();
                project = percent_encoding::percent_decode_str(parts[2])
                    .decode_utf8()
                    .map_err(|_| PluginError::invalid("The selected ZCode project ID is invalid."))?
                    .into_owned();
            }
        }
        if org.is_empty() || project.is_empty() {
            continue;
        }
        let token = decrypt(&key, &store[format!("oauth:{}:access_token", family.id())])
            .unwrap_or_default();
        if token.is_empty() || family == Site::BigModel && token == jwt {
            continue;
        }
        team = Some((family, org, project, token));
        break;
    }
    let mut state = if let Some((site, org, project, token)) = team {
        json!({"site":site.id(),"base":site.base(),"key":"","token":token,"org":org,"project":project,"jwt":jwt,"device":device})
    } else {
        json!({"site":site.id(),"base":site.base(),"key":api_key,"jwt":jwt,"device":device})
    };
    if state["key"] == "" && state["jwt"] == "" && state["org"].is_null() {
        return Err(PluginError::authentication(
            "The selected ZCode files contain no active account.",
        ));
    }
    let mut uid = String::new();
    for name in ["oauth:zai:user_info", "oauth:bigmodel:user_info"] {
        if let Some(info) =
            decrypt(&key, &store[name]).and_then(|value| serde_json::from_str::<Value>(&value).ok())
        {
            uid = identity(&info);
            if !uid.is_empty() {
                break;
            }
        }
    }
    if uid.is_empty() {
        uid = identity(&core::decode_jwt(state["jwt"].as_str().unwrap_or("")));
    }
    if uid.is_empty() {
        uid = core::fingerprint(&json!(state["jwt"]));
    }
    if state["org"].is_string() {
        uid = core::fingerprint(&json!([
            state["site"],
            state["token"],
            state["org"],
            state["project"]
        ]));
    } else if core::text(&state["key"]).is_some() {
        uid = core::fingerprint(&json!(state["key"]));
    }
    state["uid"] = json!(uid);
    Ok(state)
}
pub(crate) fn identity(user: &Value) -> String {
    ["email", "name", "user_id", "sub", "id"]
        .iter()
        .find_map(|key| core::text(&user[key]))
        .unwrap_or("")
        .trim_end_matches("@phone.local")
        .to_owned()
}
#[cfg(test)]
#[path = "../test/auth.test.rs"]
mod tests;
