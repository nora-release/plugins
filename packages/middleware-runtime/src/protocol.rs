use crate::{Action, Context, Middleware};
use nora_provider_plugin_sdk::{
    InitializeParams, PluginError, PluginResult, ProviderPlugin, RequestBodyFile,
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::io::Write;

const MAX_BODY: usize = 32 * 1024 * 1024;
struct Plugin<M>(M);
#[async_trait::async_trait]
impl<M: Middleware> ProviderPlugin for Plugin<M> {
    async fn initialize(&self, _params: InitializeParams) -> PluginResult<Value> {
        Ok(
            json!({"apiVersion":1,"capabilities":{"gatewayMiddleware":{"version":1,"bodyFile":true}}}),
        )
    }
    async fn call(&self, method: &str, params: Value) -> PluginResult<Value> {
        if method != "gatewayMiddleware" || params["version"] != 1 {
            return Err(PluginError::invalid(
                "Unsupported gateway middleware protocol.",
            ));
        }
        let phase = params["phase"]
            .as_str()
            .ok_or_else(|| PluginError::invalid("Choose a middleware phase."))?;
        if phase == "order" {
            let seats = params["seats"]
                .as_array()
                .ok_or_else(|| PluginError::invalid("Include routing seats."))?;
            let order = self
                .0
                .order(
                    params["strategy"].as_str().unwrap_or(""),
                    params["pool"].as_str().unwrap_or(""),
                    seats,
                    &params["options"],
                )
                .map_err(PluginError::invalid)?;
            return Ok(json!({"version":1,"order":order}));
        }
        if !matches!(phase, "request" | "provider" | "event" | "response") {
            return Err(PluginError::invalid("Unsupported middleware phase."));
        }
        let mut ctx: Context = serde_json::from_value(params["context"].clone())
            .map_err(|_| PluginError::invalid("Invalid middleware context."))?;
        let mut body = if let Some(file) = params.get("bodyFile") {
            if params.get("body").is_some() {
                return Err(PluginError::invalid("Choose one body source."));
            }
            let file: RequestBodyFile = serde_json::from_value(file.clone())
                .map_err(|_| PluginError::invalid("Invalid body file."))?;
            if file.length > MAX_BODY as u64 {
                return Err(PluginError::invalid("Middleware body is too large."));
            }
            serde_json::from_slice(&file.read_verified()?)
                .map_err(|_| PluginError::invalid("Invalid middleware body."))?
        } else {
            params
                .get("body")
                .cloned()
                .ok_or_else(|| PluginError::invalid("Include a middleware body."))?
        };
        let action = self
            .0
            .apply(phase, &mut body, &mut ctx)
            .map_err(PluginError::invalid)?;
        if serde_json::to_vec(&ctx.state).map_or(true, |b| b.len() > 64 * 1024) {
            return Err(PluginError::invalid("Middleware state is too large."));
        }
        let mut result = json!({"version":1,"state":ctx.state});
        match action {
            Action::Keep => result["action"] = json!("keep"),
            Action::Drop => result["action"] = json!("drop"),
            Action::Reject { status, message } => {
                result["action"] = json!("reject");
                result["reject"] = json!({"status":status,"message":message});
            }
            Action::Replace => {
                result["action"] = json!("replace");
                let bytes = serde_json::to_vec(&body)
                    .map_err(|_| PluginError::invalid("Invalid middleware result."))?;
                if bytes.len() > MAX_BODY {
                    return Err(PluginError::invalid("Middleware body is too large."));
                }
                if bytes.len() <= 512 * 1024 {
                    result["body"] = body;
                } else {
                    let path = params["outputFile"].as_str().ok_or_else(|| {
                        PluginError::invalid("The host did not offer a result file.")
                    })?;
                    let path = std::path::Path::new(path);
                    if !path.is_absolute() {
                        return Err(PluginError::invalid("Invalid middleware result file."));
                    }
                    let mut opts = std::fs::OpenOptions::new();
                    opts.write(true);
                    #[cfg(unix)]
                    {
                        use std::os::unix::fs::OpenOptionsExt;
                        opts.custom_flags(libc::O_NOFOLLOW);
                    }
                    let mut file = opts.open(path).map_err(|_| {
                        PluginError::invalid("Cannot open the middleware result file.")
                    })?;
                    if !file.metadata().is_ok_and(|m| m.is_file() && m.len() == 0) {
                        return Err(PluginError::invalid("Invalid middleware result file."));
                    }
                    file.write_all(&bytes)
                        .map_err(|_| PluginError::invalid("Cannot write the middleware result."))?;
                    result["bodyFile"] = json!({"path":path,"length":bytes.len(),"sha256":format!("{:x}",Sha256::digest(&bytes))});
                }
            }
        }
        Ok(result)
    }
}
pub async fn serve<M: Middleware>() -> std::io::Result<()> {
    nora_provider_plugin_sdk::serve(Plugin(M::default())).await
}
