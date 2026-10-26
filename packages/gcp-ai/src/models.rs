use crate::{cache, cli::Gcloud, credentials, settings::Settings};
use futures::{stream, StreamExt};
use nora_ai::{HttpClient, HttpMethod, InputModality, Model, ModelsCatalog};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub fn is_chat(id: &str) -> bool {
    id.starts_with("gemini-")
        && !id
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || c == '/')
        && ![
            "tts",
            "embedding",
            "transcribe",
            "live",
            "translate",
            "image",
            "nano-banana",
            "omni",
        ]
        .iter()
        .any(|part| id.contains(part))
}
fn cached(path: &std::path::Path) -> Option<Value> {
    let mut value = cache::read(path)?;
    let ids = value.get_mut("ids")?.as_array_mut()?;
    ids.retain(|id| id.as_str().is_some_and(is_chat));
    (!ids.is_empty()).then_some(value)
}

#[derive(Clone, Default)]
pub struct ModelLists {
    refreshing: Arc<Mutex<BTreeSet<PathBuf>>>,
}
impl ModelLists {
    pub async fn available(
        &self,
        settings: Settings,
        cli: Gcloud,
        client: Arc<dyn HttpClient>,
    ) -> PluginResult<Value> {
        let digest = cache::digest(&(
            &settings.project,
            &settings.account,
            &settings.auth_mode,
            &settings.location,
            &settings.cloudsdk_config,
        ));
        let path = settings
            .cache_root
            .join(format!("models-{}.json", &digest[..16]));
        let last = cached(&path);
        if let Some(last) = &last {
            if !settings.force {
                let fresh = cache::timestamp(last, "fetchedAt").is_some_and(|at| {
                    let age = chrono::Utc::now().timestamp_millis() - at;
                    age >= 0 && (age as f64) < settings.model_ttl * 1000.0
                });
                if !fresh {
                    let start = self
                        .refreshing
                        .lock()
                        .map(|mut refreshing| refreshing.insert(path.clone()))
                        .unwrap_or(false);
                    if start {
                        let refreshing = self.refreshing.clone();
                        let path = path.clone();
                        tokio::spawn(async move {
                            let _guard = Refreshing {
                                refreshing,
                                path: path.clone(),
                            };
                            let _ = refresh(&settings, &cli, client.as_ref(), &path).await;
                        });
                    }
                }
                return Ok(last.clone());
            }
        }
        match refresh(&settings, &cli, client.as_ref(), &path).await {
            Ok(value) => Ok(value),
            Err(error) => last.ok_or(error),
        }
    }
}
struct Refreshing {
    refreshing: Arc<Mutex<BTreeSet<PathBuf>>>,
    path: PathBuf,
}
impl Drop for Refreshing {
    fn drop(&mut self) {
        if let Ok(mut refreshing) = self.refreshing.lock() {
            refreshing.remove(&self.path);
        }
    }
}
async fn refresh(
    settings: &Settings,
    cli: &Gcloud,
    client: &dyn HttpClient,
    path: &std::path::Path,
) -> PluginResult<Value> {
    let before = cached(path).and_then(|value| cache::timestamp(&value, "fetchedAt"));
    let _lock = cache::Lock::take(path, Duration::from_secs(180)).await?;
    if let Some(value) = cached(path) {
        if cache::timestamp(&value, "fetchedAt")
            .is_some_and(|at| before.is_none_or(|before| at > before))
        {
            return Ok(value);
        }
    }
    let identity = credentials::resolve(cli, settings).await?;
    let token = cli.token(&identity).await?;
    cli.verify_owner(&identity, &token, client).await?;
    let project = identity.project.as_deref().unwrap();
    let mut names = Vec::new();
    let mut next = String::new();
    for page in 0..20 {
        let mut url =
            url::Url::parse("https://aiplatform.googleapis.com/v1beta1/publishers/google/models")
                .unwrap();
        url.query_pairs_mut().append_pair("pageSize", "200");
        if !next.is_empty() {
            url.query_pairs_mut().append_pair("pageToken", &next);
        }
        let value = credentials::json_request(
            client,
            credentials::authorized(HttpMethod::Get, url.to_string(), &token, Some(project)),
            Duration::from_secs(20),
        )
        .await?;
        for name in value
            .get("publisherModels")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|model| model["name"].as_str())
        {
            let id = name.rsplit('/').next().unwrap_or_default();
            if is_chat(id) {
                names.push(id.to_owned());
            }
        }
        next = value
            .get("nextPageToken")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .into();
        if next.is_empty() {
            break;
        }
        if page == 19 {
            return Err(PluginError::transport(
                "Vertex model catalog kept paging; an incomplete list was refused.",
            ));
        }
    }
    if names.is_empty() {
        return Err(PluginError::transport(
            "Vertex lists no chat model for this project and location.",
        ));
    }
    let mut seen = BTreeSet::new();
    names.retain(|id| seen.insert(id.clone()));
    let location = &identity.location;
    let work=stream::iter(names.clone().into_iter().map(|id|{
        let token=&token;
        async move {
            let url=format!("https://aiplatform.googleapis.com/v1beta1/projects/{project}/locations/{location}/publishers/google/models/{id}:countTokens");
            let mut request=credentials::authorized(HttpMethod::Post,url,token,Some(project)).with_header("content-type","application/json");
            request.body=Some(br#"{"contents":[{"role":"user","parts":[{"text":"x"}]}]}"#.to_vec());
            let absent=tokio::time::timeout(Duration::from_secs(8),client.stream(request)).await.ok().and_then(Result::ok).is_some_and(|response|response.status==404);
            (id,absent)
        }
    }));
    let statuses = work.buffer_unordered(6).collect::<Vec<_>>().await;
    let ids = names
        .iter()
        .filter(|id| {
            statuses
                .iter()
                .any(|(name, absent)| name == *id && !*absent)
        })
        .cloned()
        .collect::<Vec<_>>();
    // A wrong project can answer 404 for every model; it must not erase a list.
    let ids = if ids.is_empty() { names } else { ids };
    let value = json!({"project":project,"location":location,"ids":ids,"fetchedAt":chrono::Utc::now().to_rfc3339()});
    cache::write(path, &value)?;
    Ok(value)
}
pub fn catalog(settings: &Settings) -> ModelsCatalog {
    let cached = settings
        .cache_root
        .parent()
        .and_then(|parent| std::fs::read_to_string(parent.join("models.json")).ok())
        .and_then(|text| ModelsCatalog::from_json_str(&text).ok());
    cached
        .or_else(|| ModelsCatalog::bundled().ok())
        .unwrap_or_default()
}
pub fn model_rows(
    ids: &[String],
    settings: &Settings,
    catalog: &ModelsCatalog,
) -> PluginResult<Value> {
    let base = settings
        .project
        .as_deref()
        .map(|project| settings.endpoint(project))
        .transpose()?
        .unwrap_or_else(|| "https://aiplatform.googleapis.com".into());
    let mut ordered = ids
        .iter()
        .enumerate()
        .map(|(index, id)| {
            let mut model = catalog
                .model("google-vertex", id)
                .cloned()
                .unwrap_or_else(|| {
                    let mut model = Model::new(id, "google-generative-ai", "gcp-ai");
                    model.context_window = 1048576;
                    model.max_tokens = 65536;
                    model.reasoning = true;
                    model.input = vec![
                        InputModality::Text,
                        InputModality::Image,
                        InputModality::Audio,
                        InputModality::Video,
                        InputModality::Pdf,
                    ];
                    model
                });
            model.provider = "gcp-ai".into();
            model.api = "google-generative-ai".into();
            model.base_url = base.clone();
            model.info.tool_calling = Some(model.info.tool_calling != Some(false));
            (index, model)
        })
        .collect::<Vec<_>>();
    ordered.sort_by(|(left_index, left), (right_index, right)| {
        right
            .info
            .release_date
            .cmp(&left.info.release_date)
            .then_with(|| right_index.cmp(left_index))
    });
    let models = ordered
        .into_iter()
        .map(|(_, model)| model)
        .collect::<Vec<_>>();
    Ok(
        json!({"models":models,"allowUnknownModels":false,"source":"remote","defaultAPI":"google-generative-ai","defaultBaseURL":base}),
    )
}
pub fn seeds() -> Value {
    serde_json::from_str::<Value>(include_str!("../nora-provider.json"))
        .expect("bundled GCP manifest")["models"]
        .clone()
}

#[cfg(test)]
#[path = "../test/models.test.rs"]
mod tests;
