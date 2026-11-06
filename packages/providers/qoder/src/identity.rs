use crate::*;
pub(crate) fn assign(credentials: &mut Value, deployment: &Deployment) -> PluginResult<()> {
    let uid = text(credentials, "uid");
    if uid.trim() != uid || uid.is_empty() || uid.len() > 512 || uid.chars().any(char::is_control) {
        return Err(authentication(
            "Qoder did not provide a valid account identity.",
        ));
    }
    let mut identity = json!({"issuer":deployment.openapi,"subject":uid});
    if let Some(instance) = &deployment.vpc {
        identity["scope"] = json!(instance);
    }
    credentials["accountIdentity"] = identity;
    Ok(())
}
#[cfg(test)]
#[path = "../test/identity.test.rs"]
mod tests;
