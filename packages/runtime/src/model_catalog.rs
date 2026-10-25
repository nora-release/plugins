use futures::StreamExt;
use nora_ai::{
    HttpClient, HttpMethod, HttpRequest, InputModality, Model, ModelCost, ModelsCatalog,
};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::Value;
use std::collections::BTreeMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, SystemTime};

const CATALOG_URL: &str = "https://models.dev/api.json";
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);
const CHECK_EVERY: Duration = Duration::from_secs(60 * 60);
const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CATALOG_BYTES: usize = 64 * 1024 * 1024;

#[derive(Default)]
struct Cache {
    catalog: Option<ModelsCatalog>,
    checked_at: Option<SystemTime>,
    last_error: Option<String>,
}

type SharedCache = Arc<tokio::sync::Mutex<Cache>>;

pub(crate) async fn load(
    client: &dyn HttpClient,
    data_directory: &Path,
) -> PluginResult<ModelsCatalog> {
    load_at(client, data_directory, SystemTime::now()).await
}

pub(crate) fn cached(data_directory: &Path) -> PluginResult<ModelsCatalog> {
    let shared = cache_for(data_directory)?;
    if let Ok(mut cache) = shared.try_lock() {
        if let Some(catalog) = &cache.catalog {
            return Ok(catalog.clone());
        }
        let catalog =
            read_catalog(&data_directory.join("models.json")).map_err(PluginError::transport)?;
        cache.catalog = Some(catalog.clone());
        return Ok(catalog);
    }
    read_catalog(&data_directory.join("models.json")).map_err(PluginError::transport)
}

pub(crate) fn saved(data_directory: &Path) -> PluginResult<ModelsCatalog> {
    read_catalog(&data_directory.join("models.json")).map_err(PluginError::transport)
}

async fn load_at(
    client: &dyn HttpClient,
    data_directory: &Path,
    now: SystemTime,
) -> PluginResult<ModelsCatalog> {
    let shared = cache_for(data_directory)?;
    let mut cache = shared.lock().await;
    if cache
        .checked_at
        .is_some_and(|checked| elapsed(now, checked) < CHECK_EVERY)
    {
        return cached_result(&cache);
    }
    let path = data_directory.join("models.json");
    if let Ok(catalog) = read_catalog(&path) {
        cache.catalog = Some(catalog);
        cache.last_error = None;
        if std::fs::metadata(&path)
            .and_then(|metadata| metadata.modified())
            .is_ok_and(|modified| elapsed(now, modified) < STALE_AFTER)
        {
            cache.checked_at = Some(now);
            return cached_result(&cache);
        }
    }

    match download(client).await {
        Ok((bytes, catalog)) => match persist(&path, &bytes) {
            Ok(()) => {
                cache.catalog = Some(catalog);
                cache.last_error = None;
            }
            Err(error) => cache.last_error = Some(error),
        },
        Err(error) => cache.last_error = Some(error),
    }
    cache.checked_at = Some(now);
    cached_result(&cache)
}

fn cache_for(directory: &Path) -> PluginResult<SharedCache> {
    static CACHES: OnceLock<Mutex<BTreeMap<PathBuf, SharedCache>>> = OnceLock::new();
    let mut caches = CACHES
        .get_or_init(Default::default)
        .lock()
        .map_err(|_| PluginError::transport("The model catalog cache is unavailable."))?;
    Ok(caches
        .entry(directory.to_path_buf())
        .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(Cache::default())))
        .clone())
}

fn cached_result(cache: &Cache) -> PluginResult<ModelsCatalog> {
    cache.catalog.clone().ok_or_else(|| {
        PluginError::transport(
            cache
                .last_error
                .as_deref()
                .unwrap_or("The model catalog has not been downloaded yet."),
        )
    })
}

fn elapsed(now: SystemTime, earlier: SystemTime) -> Duration {
    now.duration_since(earlier).unwrap_or_default()
}

fn read_catalog(path: &Path) -> Result<ModelsCatalog, String> {
    let file = File::open(path).map_err(|_| "The saved model catalog could not be read.")?;
    let mut bytes = Vec::new();
    file.take(MAX_CATALOG_BYTES as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| "The saved model catalog could not be read.")?;
    decode(&bytes)
}

async fn download(client: &dyn HttpClient) -> Result<(Vec<u8>, ModelsCatalog), String> {
    tokio::time::timeout(DOWNLOAD_TIMEOUT, async {
        let request = HttpRequest::new(HttpMethod::Get, CATALOG_URL)
            .with_header("accept", "application/json")
            .with_header("user-agent", "Nora");
        let response = client
            .stream(request)
            .await
            .map_err(|_| "The model catalog could not be reached.")?;
        if response.status != 200 {
            return Err(format!(
                "The model catalog request returned HTTP {}.",
                response.status
            ));
        }
        let mut body = response.body;
        let mut bytes = Vec::new();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|_| "The model catalog download did not finish.")?;
            if bytes.len().saturating_add(chunk.len()) > MAX_CATALOG_BYTES {
                return Err("The model catalog is larger than supported.".into());
            }
            bytes.extend_from_slice(&chunk);
        }
        let catalog = decode(&bytes)?;
        Ok((bytes, catalog))
    })
    .await
    .map_err(|_| "The model catalog request timed out.".to_string())?
}

fn persist(path: &Path, bytes: &[u8]) -> Result<(), String> {
    static NEXT_WRITE: AtomicU64 = AtomicU64::new(0);
    let directory = path
        .parent()
        .ok_or("The model catalog directory is unavailable.")?;
    std::fs::create_dir_all(directory)
        .map_err(|_| "The model catalog directory could not be created.")?;
    let temporary = directory.join(format!(
        ".models-{}-{}.tmp",
        std::process::id(),
        NEXT_WRITE.fetch_add(1, Ordering::Relaxed)
    ));
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&temporary)
        .map_err(|_| "The model catalog could not be saved.")?;
    let result = (|| {
        file.write_all(bytes)
            .and_then(|_| file.sync_all())
            .map_err(|_| "The model catalog could not be saved.")?;
        std::fs::rename(&temporary, path).map_err(|_| "The model catalog could not be replaced.")
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result.map_err(str::to_string)
}

fn decode(bytes: &[u8]) -> Result<ModelsCatalog, String> {
    if bytes.len() > MAX_CATALOG_BYTES {
        return Err("The model catalog is larger than supported.".into());
    }
    let root: Value =
        serde_json::from_slice(bytes).map_err(|_| "The model catalog response is invalid.")?;
    let providers = root
        .as_object()
        .filter(|providers| !providers.is_empty())
        .ok_or("The model catalog response has no providers.")?;
    let mut catalog = ModelsCatalog::default();
    for (provider_id, provider) in providers {
        let Some(models) = provider.get("models").and_then(Value::as_object) else {
            continue;
        };
        for (id, row) in models {
            if let Some(model) = model_from_row(provider_id, provider, id, row) {
                catalog.insert(model);
            }
        }
    }
    if catalog.model_count() == 0 {
        return Err("The model catalog response has no models.".into());
    }
    Ok(catalog)
}

fn model_from_row(provider_id: &str, provider: &Value, key: &str, row: &Value) -> Option<Model> {
    row.as_object()?;
    let id = row.get("id").and_then(Value::as_str).unwrap_or(key);
    if id.is_empty() {
        return None;
    }
    let npm = row
        .pointer("/provider/npm")
        .or_else(|| provider.get("npm"))
        .and_then(Value::as_str)
        .unwrap_or("");
    let api = match npm {
        "@ai-sdk/anthropic" => "anthropic-messages",
        "@ai-sdk/google" | "@ai-sdk/google-generative-ai" => "google-generative-ai",
        "@ai-sdk/amazon-bedrock" => "bedrock-converse-stream",
        "@ai-sdk/azure" => "azure-openai-responses",
        "@ai-sdk/openai" => "openai-responses",
        _ => "openai-completions",
    };
    let mut model = Model::new(id, api, provider_id);
    model.name = row
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())
        .unwrap_or(id)
        .into();
    model.base_url = row
        .pointer("/provider/api")
        .or_else(|| provider.get("api"))
        .and_then(Value::as_str)
        .unwrap_or("")
        .into();
    model.reasoning = row.get("reasoning").and_then(Value::as_bool) == Some(true);
    let efforts = reasoning_levels(provider_id, provider, row);
    if !efforts.is_empty() {
        model.reasoning = true;
        model.info.reasoning_efforts = efforts;
    }
    model.info.release_date = row
        .get("release_date")
        .and_then(Value::as_str)
        .map(str::to_string);
    model.info.canonical_model_id = row
        .get("canonical_model_id")
        .and_then(Value::as_str)
        .filter(|id| !id.is_empty() && id.len() <= 512 && !id.chars().any(char::is_control))
        .map(str::to_string);
    model.info.tool_calling = row.get("tool_call").and_then(Value::as_bool);
    model.info.temperature = row.get("temperature").and_then(Value::as_bool);
    model.input = input_modalities(row);
    apply_kind(row, &mut model);
    let input = positive_limit(row.pointer("/limit/input"));
    let context = positive_limit(row.pointer("/limit/context"));
    model.context_window = match (input, context) {
        (Some(input), Some(context)) => input.min(context),
        (Some(limit), _) | (_, Some(limit)) => limit,
        _ => 0,
    };
    model.max_tokens = row
        .pointer("/limit/output")
        .and_then(Value::as_u64)
        .unwrap_or(0);
    model.cost = ModelCost {
        input: cost(row, "input"),
        output: cost(row, "output"),
        cache_read: cost(row, "cache_read"),
        cache_write: cost(row, "cache_write"),
    };
    crate::pricing::apply(row, &mut model);
    Some(model)
}

fn reasoning_levels(provider_id: &str, provider: &Value, row: &Value) -> Vec<String> {
    let own = explicit_reasoning_levels(provider_id, row);
    let options = row.get("reasoning_options").and_then(Value::as_array);
    let family = row.get("family").and_then(Value::as_str).unwrap_or("");
    if !own.is_empty()
        || row.get("reasoning").and_then(Value::as_bool) != Some(true)
        || family.is_empty()
        || options.is_some_and(|options| {
            options
                .iter()
                .any(|option| option.get("type").and_then(Value::as_str) == Some("budget_tokens"))
        })
    {
        return own;
    }
    let Some(models) = provider.get("models").and_then(Value::as_object) else {
        return own;
    };
    let release = row
        .get("release_date")
        .and_then(Value::as_str)
        .unwrap_or("");
    let id = row.get("id").and_then(Value::as_str);
    models
        .values()
        .filter(|candidate| {
            candidate.get("family").and_then(Value::as_str) == Some(family)
                && candidate.get("reasoning").and_then(Value::as_bool) == Some(true)
                && candidate.get("id").and_then(Value::as_str) != id
        })
        .filter_map(|candidate| {
            let date = candidate
                .get("release_date")
                .and_then(Value::as_str)
                .unwrap_or("");
            if !release.is_empty() && date > release {
                return None;
            }
            let levels = explicit_reasoning_levels(provider_id, candidate);
            (!levels.is_empty()).then_some((
                date,
                candidate.get("id").and_then(Value::as_str).unwrap_or(""),
                levels,
            ))
        })
        .max_by(|left, right| (left.0, left.1).cmp(&(right.0, right.1)))
        .map(|(_, _, levels)| levels)
        .unwrap_or_default()
}

fn explicit_reasoning_levels(provider_id: &str, row: &Value) -> Vec<String> {
    let Some(options) = row.get("reasoning_options").and_then(Value::as_array) else {
        return Vec::new();
    };
    let mut levels = options
        .iter()
        .rev()
        .find(|option| option.get("type").and_then(Value::as_str) == Some("effort"))
        .and_then(|option| option.get("values"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    if matches!(
        provider_id,
        "deepseek" | "kimi-code-plan-cn" | "kimi-code-plan-global"
    ) && !levels.is_empty()
        && !levels.iter().any(|level| level == "none")
        && options
            .iter()
            .any(|option| option.get("type").and_then(Value::as_str) == Some("toggle"))
    {
        levels.insert(0, "none".into());
    }
    levels
}

fn positive_limit(value: Option<&Value>) -> Option<u64> {
    value.and_then(Value::as_u64).filter(|value| *value >= 2048)
}

fn cost(row: &Value, field: &str) -> f64 {
    row.get("cost")
        .and_then(|cost| cost.get(field))
        .and_then(Value::as_f64)
        .filter(|value| value.is_finite() && *value >= 0.0)
        .unwrap_or(0.0)
}

fn input_modalities(row: &Value) -> Vec<InputModality> {
    let Some(inputs) = row
        .pointer("/modalities/input")
        .or_else(|| row.pointer("/architecture/input_modalities"))
        .or_else(|| row.get("input_modalities"))
        .and_then(Value::as_array)
    else {
        return vec![InputModality::Text];
    };
    let mut output = Vec::new();
    for input in inputs.iter().filter_map(Value::as_str) {
        let modality = match input.to_ascii_lowercase().as_str() {
            "text" => InputModality::Text,
            "image" => InputModality::Image,
            "audio" => InputModality::Audio,
            "video" => InputModality::Video,
            "pdf" => InputModality::Pdf,
            _ => continue,
        };
        if !output.contains(&modality) {
            output.push(modality);
        }
    }
    output
}

fn apply_kind(row: &Value, model: &mut Model) {
    let kind = row
        .get("kind")
        .or_else(|| row.pointer("/info/nativeKind"))
        .or_else(|| row.pointer("/capabilities/type"))
        .or_else(|| row.get("type"))
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|kind| {
            !kind.is_empty() && kind.len() <= 128 && !kind.chars().any(char::is_control)
        });
    model.info.native_kind = kind.map(str::to_owned);
    let kind = kind.unwrap_or("").to_ascii_lowercase();
    let non_chat = match kind.as_str() {
        "image" | "image_generation" | "image-generation" => {
            model.info.image_output = Some(true);
            true
        }
        "video" | "video_generation" | "video-generation" => {
            model.info.video_output = Some(true);
            true
        }
        "embedding" | "embeddings" => {
            model.info.embedding = Some(true);
            true
        }
        "decision" => {
            model.info.decision = Some(true);
            true
        }
        "audio" | "audio_generation" | "speech" | "tts" | "text_to_speech" | "text-to-speech"
        | "transcription" | "asr" | "rerank" | "reranking" | "moderation" | "completion" => true,
        _ => false,
    };
    if kind.is_empty() && model.id.to_ascii_lowercase().contains("embed") {
        model.info.embedding = Some(true);
    }
    let explicit_chat = row
        .pointer("/info/chat")
        .or_else(|| row.get("chat"))
        .or_else(|| row.pointer("/capabilities/completion_chat"))
        .and_then(Value::as_bool)
        .or_else(|| (kind == "chat").then_some(true));
    let outputs = row
        .pointer("/modalities/output")
        .or_else(|| row.pointer("/architecture/output_modalities"))
        .or_else(|| row.get("output_modalities"))
        .and_then(Value::as_array);
    let text_output = outputs
        .filter(|outputs| !outputs.is_empty())
        .map(|outputs| outputs.iter().any(|output| output == "text"));
    model.info.chat = if non_chat || explicit_chat == Some(false) {
        Some(false)
    } else {
        explicit_chat
            .or_else(|| (kind == "chat").then_some(true))
            .or(text_output)
    };
    if outputs.is_some_and(|outputs| {
        outputs
            .iter()
            .any(|output| output == "embedding" || output == "embeddings")
    }) {
        model.info.embedding = Some(true);
    }
    crate::models::apply_media_metadata(row, model);
    if model.info.embedding == Some(true) && explicit_chat != Some(true) {
        model.info.chat = Some(false);
    }
    if model.info.chat.is_none() && !is_chat_model(&model.id, row) {
        model.info.chat = Some(false);
    }
    if model.info.chat == Some(false) {
        model.info.tool_calling = Some(false);
    }
}

fn is_chat_model(id: &str, row: &Value) -> bool {
    if row
        .pointer("/modalities/output")
        .and_then(Value::as_array)
        .is_some_and(|outputs| {
            !outputs.is_empty() && !outputs.iter().any(|output| output == "text")
        })
    {
        return false;
    }
    ![
        "embed",
        "-tts",
        "image",
        "audio",
        "-live",
        "robotics",
        "computer-use",
        "deep-research",
        "transcribe",
        "realtime",
        "moderation",
        "whisper",
        "dall-e",
        "sora",
    ]
    .iter()
    .any(|fragment| id.contains(fragment))
}

#[cfg(test)]
#[path = "../test/model_catalog.test.rs"]
mod tests;
