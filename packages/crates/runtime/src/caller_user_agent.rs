use crate::definition::Definition;
use serde_json::Value;
use std::collections::BTreeMap;

pub(crate) fn apply(
    definition: &Definition,
    credentials: &Value,
    caller: Option<&str>,
    headers: &mut BTreeMap<String, String>,
) {
    if !definition.config.pass_user_agent
        || definition.config.own_client
        || !crate::auth::is_api(credentials)
    {
        return;
    }
    let Some(caller) = caller.filter(|value| {
        !value.is_empty()
            && value.len() <= 8192
            && !value.chars().any(char::is_control)
            && !["nora/", "nora-", "magpie/", "magpie-"]
                .iter()
                .any(|prefix| value.to_ascii_lowercase().starts_with(prefix))
    }) else {
        return;
    };
    // The user's configured headers take precedence, irrespective of casing.
    headers.retain(|name, _| !name.eq_ignore_ascii_case("user-agent"));
    if !definition
        .config
        .headers
        .keys()
        .any(|name| name.eq_ignore_ascii_case("user-agent"))
    {
        headers.insert("User-Agent".into(), caller.into());
    }
}

#[cfg(test)]
#[path = "../test/caller_user_agent.test.rs"]
mod tests;
