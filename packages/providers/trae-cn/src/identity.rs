use crate::*;
pub(crate) fn assign(site: Site, credentials: &mut Value) -> PluginResult<()> {
    let uid = text(credentials, "uid");
    if uid.trim() != uid || uid.is_empty() || uid.len() > 512 || uid.chars().any(char::is_control) {
        return Err(signed_out());
    }
    credentials["accountIdentity"] = json!({"issuer":site.auth(),"subject":uid});
    Ok(())
}
#[cfg(test)]
#[path = "../test/identity.test.rs"]
mod tests;
