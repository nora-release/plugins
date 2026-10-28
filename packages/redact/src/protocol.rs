use crate::{Engine, Settings, StreamState};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use nora_provider_plugin_sdk::{
    InitializeParams, PluginError, PluginResult, ProviderPlugin, RequestBodyFile,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::Path,
    sync::Mutex,
};
const MAX: usize = 32 * 1024 * 1024;
#[derive(Default)]
pub struct Plugin {
    engine: Mutex<Option<Engine>>,
}
#[async_trait::async_trait]
impl ProviderPlugin for Plugin {
    async fn initialize(&self, params: InitializeParams) -> PluginResult<Value> {
        let path = Path::new(&params.data_directory).join("masking.key");
        let key = load_key(&path).map_err(PluginError::invalid)?;
        *self
            .engine
            .lock()
            .map_err(|_| PluginError::invalid("Masking is unavailable."))? = Some(Engine::new(key));
        Ok(json!({"apiVersion":1,"capabilities":{"gatewayMasker":{"version":1,"bodyFile":true}}}))
    }
    async fn call(&self, method: &str, params: Value) -> PluginResult<Value> {
        if method != "gatewayMasker" || params["version"] != 1 {
            return Err(PluginError::invalid("Unsupported masking contract."));
        }
        let payload = read_payload(&params)?;
        let mut guard = self
            .engine
            .lock()
            .map_err(|_| PluginError::invalid("Masking is unavailable."))?;
        let engine = guard
            .as_mut()
            .ok_or_else(|| PluginError::invalid("Initialize masking before use."))?;
        let result = match payload["operation"].as_str() {
            Some("settings" | "preview" | "mask") => {
                let mut settings: Settings = serde_json::from_value(
                    payload
                        .get("settings")
                        .cloned()
                        .unwrap_or_else(|| json!({})),
                )
                .map_err(|_| PluginError::invalid("Invalid masking settings."))?;
                settings = settings.validate().map_err(PluginError::invalid)?;
                match payload["operation"].as_str().unwrap() {
                    "settings" => {
                        let mut v = settings.metadata();
                        v["settings"] = json!(settings);
                        v
                    }
                    "preview" => {
                        let text = payload["text"]
                            .as_str()
                            .ok_or_else(|| PluginError::invalid("Include preview text."))?;
                        let mut end = text.len().min(64 * 1024);
                        while !text.is_char_boundary(end) {
                            end -= 1
                        }
                        let off = !settings.wanted();
                        if off {
                            settings.redact = true;
                            settings.redact_personal = true
                        }
                        let text = engine.preview(&text[..end], &settings);
                        json!({"text":text,"hits":engine.hits,"off":off})
                    }
                    _ => {
                        let text = payload["text"]
                            .as_str()
                            .filter(|s| s.len() <= MAX)
                            .ok_or_else(|| {
                                PluginError::invalid("Include text smaller than 32 MiB.")
                            })?;
                        let mode = payload["kind"].as_str().unwrap_or("json");
                        if !matches!(mode, "json" | "text") {
                            return Err(PluginError::invalid("Invalid masking input format."));
                        }
                        let text = engine.mask(text, &settings, mode == "json");
                        json!({"text":text,"hits":engine.hits,"blocked":engine.hits.iter().any(|h|h.action=="block")})
                    }
                }
            }
            Some("restore") => {
                let bytes = payload["bytes"]
                    .as_str()
                    .and_then(|s| STANDARD.decode(s).ok())
                    .filter(|b| b.len() <= MAX)
                    .ok_or_else(|| PluginError::invalid("Invalid masking response bytes."))?;
                let mut state = if payload["state"].is_null() {
                    StreamState::new(
                        payload["contentType"].as_str().unwrap_or(""),
                        payload["contentEncoding"].as_str().unwrap_or(""),
                    )
                } else {
                    serde_json::from_value(payload["state"].clone())
                        .map_err(|_| PluginError::invalid("Invalid masking stream state."))?
                };
                let bytes = state
                    .push(engine, &bytes, payload["finish"].as_bool().unwrap_or(false))
                    .map_err(PluginError::invalid)?;
                json!({"bytes":STANDARD.encode(bytes),"state":state})
            }
            _ => return Err(PluginError::invalid("Unsupported masking operation.")),
        };
        write_result(result, &params)
    }
}
fn read_payload(params: &Value) -> PluginResult<Value> {
    let Some(file) = params.get("payloadFile") else {
        return Ok(params.clone());
    };
    if params.get("operation").is_some() {
        return Err(PluginError::invalid("Choose one masking input source."));
    }
    let file: RequestBodyFile = serde_json::from_value(file.clone())
        .map_err(|_| PluginError::invalid("Invalid masking input file."))?;
    if file.length > MAX as u64 * 2 {
        return Err(PluginError::invalid("Masking input exceeds its limit."));
    }
    serde_json::from_slice(&file.read_verified()?)
        .map_err(|_| PluginError::invalid("Invalid masking input."))
}
fn write_result(mut result: Value, params: &Value) -> PluginResult<Value> {
    result["version"] = json!(1);
    let bytes =
        serde_json::to_vec(&result).map_err(|_| PluginError::invalid("Invalid masking output."))?;
    if bytes.len() <= 512 * 1024 {
        return Ok(result);
    }
    if bytes.len() > MAX * 2 {
        return Err(PluginError::invalid("Masking output exceeds its limit."));
    }
    let path = params["outputFile"]
        .as_str()
        .ok_or_else(|| PluginError::invalid("Include a masking result file."))?;
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err(PluginError::invalid("Invalid masking result path."));
    }
    let mut o = std::fs::OpenOptions::new();
    o.write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = o
        .open(path)
        .map_err(|_| PluginError::invalid("Cannot open the masking result file."))?;
    if !file.metadata().is_ok_and(|m| m.is_file() && m.len() == 0) {
        return Err(PluginError::invalid("Invalid masking result file."));
    }
    file.write_all(&bytes)
        .map_err(|_| PluginError::invalid("Cannot write the masking result."))?;
    Ok(
        json!({"version":1,"resultFile":{"path":path,"length":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))}}),
    )
}
fn load_key(path: &Path) -> Result<[u8; 32], String> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|_| "Cannot open masking storage.")?;
    }
    let read = || -> Result<[u8; 32], String> {
        let mut opts = std::fs::OpenOptions::new();
        opts.read(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.custom_flags(libc::O_NOFOLLOW);
        }
        let mut file = opts
            .open(path)
            .map_err(|_| "Cannot read the masking key.")?;
        if !file.metadata().is_ok_and(|m| m.is_file() && m.len() == 32) {
            return Err("The stored masking key is invalid.".into());
        }
        let mut key = [0; 32];
        file.read_exact(&mut key)
            .map_err(|_| "Cannot read the masking key.")?;
        Ok(key)
    };
    if path.symlink_metadata().is_ok() {
        return read();
    }
    let mut key = [0; 32];
    getrandom::getrandom(&mut key).map_err(|_| "Cannot create a masking key.")?;
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    match opts.open(path) {
        Ok(mut file) => {
            file.write_all(&key)
                .and_then(|_| file.sync_all())
                .map_err(|_| "Cannot save the masking key.")?;
            Ok(key)
        }
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => read(),
        Err(_) => Err("Cannot save the masking key.".into()),
    }
}
pub async fn serve() -> std::io::Result<()> {
    nora_provider_plugin_sdk::serve(Plugin::default()).await
}
