use crate::signing::Site;
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
pub(crate) fn assign(site: Site, credentials: &mut Value) -> PluginResult<bool> {
    let Some(uid) = credentials["realUserID"].as_str().filter(|v| !v.is_empty()) else {
        return Ok(false);
    };
    if uid.trim() != uid || uid.len() > 512 || uid.chars().any(char::is_control) {
        return Err(PluginError::authentication(
            "MiniMax Code did not provide a valid account identity.",
        ));
    }
    credentials["accountIdentity"] = json!({"issuer":site.account,"subject":uid});
    Ok(true)
}
#[cfg(test)]
#[path = "../test/identity.test.rs"]
mod tests;
