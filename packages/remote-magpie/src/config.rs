use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use url::Url;

#[derive(Clone)]
pub(crate) struct Account {
    pub root: String,
    pub key: String,
    pub name: String,
}
impl Account {
    pub fn inputs(value: &Value) -> PluginResult<Self> {
        let value = value.get("inputs").unwrap_or(value);
        let root = normalize(
            value
                .get("endpoint")
                .or_else(|| value.get("baseURL"))
                .and_then(Value::as_str)
                .ok_or_else(|| PluginError::invalid("Enter the remote gateway address."))?,
        )?;
        let key = value
            .get("apiKey")
            .and_then(Value::as_str)
            .filter(|key| {
                !key.is_empty()
                    && key.len() <= 8192
                    && key.trim() == *key
                    && !key.chars().any(char::is_control)
            })
            .ok_or_else(|| PluginError::invalid("Enter an enabled gateway API key."))?
            .to_owned();
        let name = value
            .get("name")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|name| {
                !name.is_empty() && name.len() <= 256 && !name.chars().any(char::is_control)
            })
            .map(str::to_owned)
            .unwrap_or_else(|| Url::parse(&root).unwrap().host_str().unwrap().to_owned());
        Ok(Self { root, key, name })
    }
    pub fn credentials(value: Option<&Value>) -> PluginResult<Self> {
        let value = value.ok_or_else(|| {
            PluginError::authentication("Connect the remote gateway with its address and API key.")
        })?;
        let root = value
            .pointer("/extras/endpoint")
            .or_else(|| value.get("endpoint"))
            .and_then(Value::as_str)
            .ok_or_else(|| PluginError::authentication("Reconnect this remote gateway."))?;
        let key = value
            .get("key")
            .or_else(|| value.get("access"))
            .and_then(Value::as_str)
            .ok_or_else(|| PluginError::authentication("Reconnect this remote gateway."))?;
        Self::inputs(
            &json!({"endpoint":root,"apiKey":key,"name":value.get("label").or_else(||value.pointer("/extras/remoteName"))}),
        )
    }
    pub fn saved(&self) -> Value {
        json!({"type":"api","authMethod":"apiKey","configured":true,"key":self.key,"label":self.name,"extras":{"endpoint":self.root,"remoteName":self.name}})
    }
    pub fn fingerprint(&self) -> String {
        format!(
            "{:x}",
            Sha256::digest(format!("{}\0{}", self.root, self.key).as_bytes())
        )
    }
    pub fn at(&self, path: &str) -> String {
        format!("{}{}", self.root, path)
    }
    pub fn target(&self, raw: &str) -> PluginResult<Url> {
        let url = Url::parse(raw)
            .map_err(|_| PluginError::invalid("The remote request address is invalid."))?;
        let base = Url::parse(&self.root).unwrap();
        if url.origin() != base.origin()
            || url.username() != ""
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(PluginError::invalid(
                "The request does not belong to this remote gateway.",
            ));
        }
        let path = url
            .path()
            .strip_prefix(base.path().trim_end_matches('/'))
            .ok_or_else(|| {
                PluginError::invalid("The request does not belong to this remote gateway.")
            })?;
        if !supported_path(path) {
            return Err(PluginError::invalid(
                "This remote gateway operation is not supported.",
            ));
        }
        Ok(url)
    }
}
pub(crate) fn normalize(raw: &str) -> PluginResult<String> {
    let raw = raw.trim().trim_end_matches('/');
    if raw.is_empty() || raw.len() > 8192 || raw.chars().any(char::is_control) {
        return Err(PluginError::invalid(
            "Enter a valid HTTP or HTTPS gateway address.",
        ));
    }
    let raw = if raw.contains("://") {
        raw.to_owned()
    } else {
        format!("http://{raw}")
    };
    let mut url = Url::parse(&raw)
        .map_err(|_| PluginError::invalid("Enter a valid HTTP or HTTPS gateway address."))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(PluginError::invalid(
            "Use an HTTP or HTTPS gateway address without a password, query, or fragment.",
        ));
    }
    let mut path = url.path().trim_end_matches('/').to_owned();
    for suffix in [
        "/v1/messages/count_tokens",
        "/v1/messages",
        "/v1/chat/completions",
        "/v1/responses",
        "/v1/systemone",
        "/v1/embeddings",
        "/v1/rerank",
        "/v1/audio/transcriptions",
        "/v1/audio/translations",
        "/v1/images/generations",
        "/v1/images/edits",
        "/v1/videos",
        "/v1",
    ] {
        if let Some(root) = path.strip_suffix(suffix) {
            path = root.into();
            break;
        }
    }
    url.set_path(&path);
    Ok(url.as_str().trim_end_matches('/').to_owned())
}
fn supported_path(path: &str) -> bool {
    if [
        "/v1/chat/completions",
        "/v1/responses",
        "/v1/responses/compact",
        "/v1/messages",
        "/v1/messages/count_tokens",
        "/v1/images/generations",
        "/v1/images/edits",
        "/v1/videos",
        "/v1/embeddings",
        "/v1/rerank",
        "/v1/systemone",
        "/v1/audio/transcriptions",
        "/v1/audio/translations",
    ]
    .contains(&path)
    {
        return true;
    }
    let Some(tail) = path.strip_prefix("/v1/videos/") else {
        return false;
    };
    let id = tail.strip_suffix("/content").unwrap_or(tail);
    !id.is_empty()
        && id.len() <= 12288
        && !id.contains('/')
        && !id.contains(['\\', '\0'])
        && !matches!(id, "." | "..")
}
#[cfg(test)]
#[path = "../test/config.test.rs"]
mod tests;
