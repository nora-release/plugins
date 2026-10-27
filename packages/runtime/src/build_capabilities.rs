use serde_json::{json, Value};

/// The build declares what its package can expose. The host must still intersect
/// this with the executable's independently returned SDK feature versions.
pub(crate) fn declare(manifest: &mut Value) {
    // Pure request processors do not run provider account or network services.
    if manifest["api"] == "gateway-middleware" {
        return;
    }
    let alpha_search = manifest.get("id").and_then(Value::as_str) == Some("openai");
    let learning_usage = manifest.get("id").and_then(Value::as_str) == Some("anthropic");
    let account_revocation = manifest.get("id").and_then(Value::as_str) == Some("chatgpt-api");
    if !manifest["config"].is_object() {
        manifest["config"] = json!({});
    }
    let config = &mut manifest["config"];
    let mut features = json!({"subscriptionPolicy":1,"automaticProxy":1});
    if account_revocation {
        features["accountRevocation"] = json!(1);
        config["accountRevocation"] = json!({"version":1});
    }
    if learning_usage {
        features["learningUsage"] = json!(1);
        config["learningUsage"] = json!({"version":1});
        features["sessionActivity"] = json!(1);
        config["sessionActivity"] = json!({"version":1});
    }
    if alpha_search {
        features["alphaSearch"] = json!(1);
        features["clientObservation"] = json!(1);
    }
    if config["connectionOptions"] == true {
        features["connectionOverrides"] = json!(1);
    }
    if config["accountImport"]["formats"]
        .as_array()
        .is_some_and(|formats| !formats.is_empty())
    {
        features["accountImport"] = json!(1);
    }
    if config["accountAutomation"]["version"] == 1 {
        features["accountAutomation"] = json!(1);
    }
    if config["modelResolutionModels"]
        .as_array()
        .is_some_and(|models| !models.is_empty())
    {
        features["modelResolution"] = json!(1);
    }
    if config["searchAPI"]["version"] == 1 || config["ownSearch"]["version"] == 1 {
        features["webSearch"] = json!(1);
    }
    config["sdkCapabilities"] = features;
}

#[cfg(test)]
#[path = "../test/build_capabilities.test.rs"]
mod tests;

#[cfg(test)]
#[path = "../test/compiled_capabilities.test.rs"]
mod compiled_tests;
