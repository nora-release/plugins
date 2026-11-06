use crate::{
    cache,
    cli::Gcloud,
    settings::{validate_account, validate_project, Settings},
};
use nora_ai::{HttpClient, HttpMethod, HttpRequest};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub fn default_gcloud() -> PathBuf {
    if cfg!(windows) {
        if let Some(appdata) = std::env::var_os("APPDATA") {
            return PathBuf::from(appdata).join("gcloud");
        }
    }
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .unwrap_or_default()
        .join(".config/gcloud")
}
pub fn adc_file(settings: &Settings) -> PathBuf {
    if let Some(directory) = &settings.cloudsdk_config {
        return directory.join("application_default_credentials.json");
    }
    if let Some(path) =
        std::env::var_os("GOOGLE_APPLICATION_CREDENTIALS").filter(|path| !path.is_empty())
    {
        return PathBuf::from(path);
    }
    default_gcloud().join("application_default_credentials.json")
}
pub fn has_adc(settings: &Settings) -> bool {
    std::fs::read(adc_file(settings)).is_ok()
}
pub fn effective_mode(settings: &Settings) -> &str {
    if settings.auth_mode == "auto" {
        if has_adc(settings) {
            "adc"
        } else {
            "gcloud"
        }
    } else {
        &settings.auth_mode
    }
}
fn configured(value: String) -> Option<String> {
    (!value.is_empty() && value != "(unset)").then_some(value)
}
pub async fn resolve_account(cli: &Gcloud, settings: &Settings) -> PluginResult<String> {
    let account = if let Some(account) = &settings.account {
        account.clone()
    } else {
        configured(
            cli.run(
                &["config".into(), "get-value".into(), "account".into()],
                settings,
            )
            .await?,
        )
        .ok_or_else(|| {
            PluginError::authentication(
                "No Google Cloud account is signed in. Run gcloud auth login in a terminal.",
            )
        })?
    };
    validate_account(&account)?;
    Ok(account)
}
pub async fn resolve(cli: &Gcloud, settings: &Settings) -> PluginResult<Settings> {
    let mut identity = settings.clone();
    identity.auth_mode = effective_mode(settings).into();
    if identity.project.is_none() {
        identity.project = configured(
            cli.run(
                &["config".into(), "get-value".into(), "project".into()],
                &identity,
            )
            .await?,
        );
    }
    validate_project(identity.project.as_deref().ok_or_else(|| {
        PluginError::authentication(
            "No Google Cloud project is configured. Set the project selected in Cloud Console.",
        )
    })?)?;
    if identity.auth_mode == "gcloud" {
        identity.account = Some(resolve_account(cli, &identity).await?);
    }
    Ok(identity)
}
pub async fn json_request(
    client: &dyn HttpClient,
    request: HttpRequest,
    timeout: Duration,
) -> PluginResult<Value> {
    tokio::time::timeout(timeout, async {
        let response = client.stream(request).await.map_err(|_| {
            PluginError::transport("The Google Cloud service could not be reached.")
        })?;
        if !(200..300).contains(&response.status) {
            return Err(PluginError::transport(format!(
                "Google Cloud returned HTTP {}.",
                response.status
            )));
        }
        let text = nora_ai::collect_http_body_text_limited(response.body, 8 * 1024 * 1024)
            .await
            .map_err(|_| {
                PluginError::transport("The Google Cloud response is incomplete or too large.")
            })?;
        serde_json::from_str(&text)
            .map_err(|_| PluginError::transport("The Google Cloud response is invalid."))
    })
    .await
    .map_err(|_| PluginError::transport("The Google Cloud service did not answer in time."))?
}
pub fn authorized(
    method: HttpMethod,
    url: impl Into<String>,
    token: &str,
    project: Option<&str>,
) -> HttpRequest {
    let mut request =
        HttpRequest::new(method, url).with_header("authorization", format!("Bearer {token}"));
    if let Some(project) = project {
        request = request.with_header("x-goog-user-project", project);
    }
    request
}
pub async fn account_of(client: &dyn HttpClient, token: &str) -> Option<String> {
    let value = json_request(
        client,
        authorized(
            HttpMethod::Get,
            "https://openidconnect.googleapis.com/v1/userinfo",
            token,
            None,
        ),
        Duration::from_secs(10),
    )
    .await
    .ok()?;
    let account = value.get("email")?.as_str()?;
    validate_account(account).ok()?;
    Some(account.into())
}
pub async fn owner(cli: &Gcloud, settings: &Settings, client: &dyn HttpClient) -> Option<String> {
    let mut settings = settings.clone();
    settings.auth_mode = "adc".into();
    account_of(client, &cli.token(&settings).await.ok()?).await
}
pub async fn usable_projects(client: &dyn HttpClient, token: &str) -> PluginResult<Vec<Value>> {
    use futures::{stream, StreamExt};
    let value = json_request(
        client,
        authorized(
            HttpMethod::Get,
            "https://cloudresourcemanager.googleapis.com/v1/projects",
            token,
            None,
        ),
        Duration::from_secs(15),
    )
    .await?;
    let projects = value
        .get("projects")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|project| {
            project["lifecycleState"] == "ACTIVE"
                && project["projectId"]
                    .as_str()
                    .is_some_and(|id| !id.is_empty())
        })
        .cloned()
        .collect::<Vec<_>>();
    let work=stream::iter(projects.into_iter().enumerate().map(|(index,project)|async move {
        let id=project["projectId"].as_str().unwrap();
        let url=format!("https://serviceusage.googleapis.com/v1/projects/{}/services/aiplatform.googleapis.com",url::form_urlencoded::byte_serialize(id.as_bytes()).collect::<String>());
        let enabled=json_request(client,authorized(HttpMethod::Get,url,token,None),Duration::from_secs(15)).await.ok().is_some_and(|value|value["state"]=="ENABLED");
        (index,json!({"projectId":id,"name":project["name"].as_str().filter(|name|!name.is_empty()).unwrap_or(id),"vertexEnabled":enabled}))
    }));
    let mut found = work.buffer_unordered(6).collect::<Vec<_>>().await;
    found.sort_by_key(|(index, _)| *index);
    Ok(found.into_iter().map(|(_, project)| project).collect())
}
pub fn choose_project(
    projects: &[Value],
    settings: &Settings,
    account: &str,
) -> PluginResult<String> {
    if let Some(project) = &settings.project {
        validate_project(project)?;
        if projects
            .iter()
            .any(|value| value["projectId"] == *project && value["vertexEnabled"] != true)
        {
            return Err(PluginError::authentication(format!("{project} does not have Vertex AI enabled, so nothing was registered. Enable aiplatform.googleapis.com and sign in again.")));
        }
        return Ok(project.clone());
    }
    let project=projects.iter().find(|project|project["vertexEnabled"]==true).and_then(|project|project["projectId"].as_str()).ok_or_else(||PluginError::authentication(format!("No Google Cloud project for {account} has Vertex AI enabled, so nothing was registered. Create a project and enable aiplatform.googleapis.com before signing in again.")))?;
    validate_project(project)?;
    Ok(project.into())
}
pub fn slug(account: &str) -> String {
    let mut result = String::new();
    let mut separator = false;
    for character in account.chars() {
        if character.is_ascii_alphanumeric() {
            result.push(character.to_ascii_lowercase());
            separator = false;
        } else if !separator {
            result.push('-');
            separator = true;
        }
    }
    result
}
fn plain(account: &str, root: &Path) -> PathBuf {
    root.join(format!("adc-{}", slug(account)))
}
fn disambiguated(account: &str, root: &Path) -> PathBuf {
    use sha2::{Digest, Sha256};
    root.join(format!(
        "adc-{}-{}",
        slug(account),
        &format!("{:x}", Sha256::digest(account.to_lowercase().as_bytes()))[..6]
    ))
}
pub async fn directory_owned(
    account: &str,
    settings: &Settings,
    cli: &Gcloud,
    client: &dyn HttpClient,
) -> PathBuf {
    let plain = plain(account, &settings.adc_root);
    let mut probe = settings.clone();
    probe.cloudsdk_config = Some(plain.clone());
    if has_adc(&probe) {
        if owner(cli, &probe, client)
            .await
            .is_none_or(|owner| owner.eq_ignore_ascii_case(account))
        {
            return plain;
        }
        return disambiguated(account, &settings.adc_root);
    }
    let hashed = disambiguated(account, &settings.adc_root);
    probe.cloudsdk_config = Some(hashed.clone());
    if has_adc(&probe) {
        hashed
    } else {
        plain
    }
}
pub async fn directory_to_create(
    account: &str,
    settings: &Settings,
    cli: &Gcloud,
    client: &dyn HttpClient,
) -> PathBuf {
    let path = plain(account, &settings.adc_root);
    if !path.exists() {
        return path;
    }
    let mut probe = settings.clone();
    probe.cloudsdk_config = Some(path.clone());
    if owner(cli, &probe, client)
        .await
        .is_none_or(|owner| owner.eq_ignore_ascii_case(account))
    {
        path
    } else {
        disambiguated(account, &settings.adc_root)
    }
}
pub fn known_accounts(settings: &Settings) -> Vec<String> {
    std::fs::read_dir(&settings.adc_root)
        .into_iter()
        .flatten()
        .filter_map(Result::ok)
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            (name.starts_with("adc-")
                && !name.starts_with("adc-pending-")
                && entry
                    .path()
                    .join("application_default_credentials.json")
                    .is_file())
            .then(|| name[4..].to_owned())
        })
        .collect()
}
pub fn sign_in_url(consent: Option<&str>, known: bool) -> String {
    let Some(consent) = consent else {
        return if known {
            "https://accounts.google.com/AddSession?hl=en&continue=https%3A%2F%2Fconsole.cloud.google.com%2F".into()
        } else {
            "https://console.cloud.google.com/?hl=en".into()
        };
    };
    if !known {
        return consent.into();
    }
    let mut url = url::Url::parse("https://accounts.google.com/AddSession?hl=en").unwrap();
    url.query_pairs_mut().append_pair("continue", consent);
    url.into()
}
pub fn record(settings: &Settings, warning: Option<&str>) -> Value {
    let mut extras = json!({"source":"gcloud","authMode":settings.auth_mode,"project":settings.project,"profile":settings.profile,"billingAccount":settings.billing_account,"cloudsdkConfig":settings.cloudsdk_config,"creditsSession":if warning.is_some(){"unavailable"}else{"ready"}});
    if let Some(account) = &settings.account {
        extras["accountId"] = json!(account);
        extras["email"] = json!(account);
    }
    let mut credentials = json!({"type":"oauth","authMethod":"gcloud","access":"","refresh":"","expires":0,"extras":extras});
    if let Some(account) = &settings.account {
        credentials["accountIdentity"] = json!({"subject":account.to_lowercase()});
    }
    credentials
}

pub async fn start_login(
    mut settings: Settings,
    cli: &Gcloud,
    client: &dyn HttpClient,
    browser: &crate::browser::BrowserService,
) -> PluginResult<Value> {
    let pinned = settings.cloudsdk_config.is_some() && settings.account.is_none();
    if pinned && settings.auth_mode != "gcloud" {
        settings.auth_mode = "adc".into();
        let mut identity = resolve(cli, &settings).await?;
        identity.account=Some(owner(cli,&identity,client).await.ok_or_else(||PluginError::authentication("The existing ADC was kept, but its Google account could not be confirmed; no account was recorded."))?);
        let warning = "Existing ADC imported; the Chrome Console session was not checked.";
        return Ok(
            json!({"ok":true,"credentials":record(&identity,Some(warning)),"warning":warning}),
        );
    }
    let known = !known_accounts(&settings).is_empty();
    if settings.auth_mode == "gcloud" {
        let mut identity = resolve(cli, &settings).await?;
        let _ = cli.token(&identity).await?;
        let window = browser
            .open_signin(&settings.profile, &sign_in_url(None, known))
            .await;
        let warning = match window {
            Ok(mut window) => {
                let result = window.wait().await;
                window.close().await;
                result.err().map(|error| error.to_string())
            }
            Err(error) => Some(error.to_string()),
        };
        if identity.account.is_none() {
            identity.account = owner(cli, &identity, client).await;
        }
        return Ok(
            json!({"ok":true,"credentials":record(&identity,warning.as_deref()),"warning":warning}),
        );
    }
    cache::directory(&settings.adc_root)?;
    let directory = if let Some(account) = &settings.account {
        directory_to_create(account, &settings, cli, client).await
    } else {
        settings
            .adc_root
            .join(format!("adc-pending-{}", uuid::Uuid::new_v4()))
    };
    if directory == default_gcloud() {
        return Err(PluginError::invalid(
            "Cannot create sign-in credentials in the machine's default gcloud directory.",
        ));
    }
    let mut local = settings.clone();
    local.cloudsdk_config = Some(directory.clone());
    local.auth_mode = "adc".into();
    if has_adc(&local) {
        return Err(PluginError::authentication("That account was already signed in, so no new ADC was added. Choose a different Google account."));
    }
    let created_directory = !directory.exists();
    cache::directory(&directory)?;
    let mut cleanup = NewAdc {
        directory: directory.clone(),
        created_directory,
        created_file: true,
        keep: false,
    };
    let mut consent = cli.begin_adc(&local).await?;
    cleanup.created_file = consent.created_file();
    let mut window = browser
        .open_signin(&settings.profile, &sign_in_url(Some(&consent.url), known))
        .await?;
    let consent_result = tokio::time::timeout(Duration::from_secs(600), consent.finish())
        .await
        .map_err(|_| {
            PluginError::authentication("Google sign-in did not finish within ten minutes.")
        });
    if !matches!(consent_result, Ok(Ok(()))) {
        consent.cancel().await;
        window.close().await;
        return consent_result.and_then(|value| value).map(|_| Value::Null);
    }
    // Keep the consent window until account and project checks have completed.
    let result=async {
        if !consent.created_file()||!has_adc(&local){return Err(PluginError::authentication("Google sign-in created no new readable ADC; no account was added."));}
        let account=match owner(cli,&local,client).await {
            Some(account)=>account.to_lowercase(),
            None=>{if settings.account.is_some(){cleanup.keep=true;return Err(PluginError::authentication("The ADC was kept, but its Google account could not be confirmed; no new account was recorded."));}return Err(PluginError::authentication("The Google account identity could not be confirmed after consent; this run's credentials were removed."));}
        };
        let existing=directory_owned(&account,&settings,cli,client).await;
        if existing!=directory&&existing.join("application_default_credentials.json").is_file(){return Err(PluginError::authentication(format!("{account} was already signed in; choose a different Google account.")));}
        if settings.account.as_ref().is_some_and(|wanted|!wanted.eq_ignore_ascii_case(&account)){return Err(PluginError::authentication("The consent page approved a different Google account; no new account or ADC was saved."));}
        if settings.account.is_none(){
            let own=directory_to_create(&account,&settings,cli,client).await;
            if own.exists(){return Err(PluginError::authentication(format!("{account} was already signed in or its ADC directory already exists; nothing was replaced.")));}
            std::fs::rename(&directory,&own).map_err(|_|PluginError::transport("The new Google account directory could not be saved."))?;
            cleanup.directory=own.clone();local.cloudsdk_config=Some(own);
        }
        if settings.project.is_none(){let token=cli.token(&local).await?;let projects=usable_projects(client,&token).await.map_err(|_|PluginError::authentication("Google Cloud projects could not be listed; no account was registered. Try again or select the project explicitly."))?;settings.project=Some(choose_project(&projects,&settings,&account)?);}
        local.project=settings.project.clone();local.account=Some(account);
        cleanup.keep=true;
        Ok(json!({"ok":true,"credentials":record(&local,None)}))
    }.await;
    window.close().await;
    result
}
struct NewAdc {
    directory: PathBuf,
    created_directory: bool,
    created_file: bool,
    keep: bool,
}
impl Drop for NewAdc {
    fn drop(&mut self) {
        if self.keep || self.directory == default_gcloud() {
            return;
        }
        if self.created_file {
            let _ =
                std::fs::remove_file(self.directory.join("application_default_credentials.json"));
        }
        if self.created_directory {
            let _ = std::fs::remove_dir_all(&self.directory);
        }
    }
}

#[cfg(test)]
#[path = "../test/credentials.test.rs"]
mod tests;
