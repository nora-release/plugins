use serde_json::{json, Value};

/// The build declares what its package can expose. The host must still intersect
/// this with the executable's independently returned SDK feature versions.
pub(crate) fn declare(manifest: &mut Value) {
    if !manifest["config"].is_object() {
        manifest["config"] = json!({});
    }
    let config = &mut manifest["config"];
    let mut features = json!({"subscriptionPolicy":1,"automaticProxy":1});
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
    config["sdkCapabilities"] = features;
}

#[cfg(test)]
#[path = "../test/build_capabilities.test.rs"]
mod tests;

#[cfg(test)]
#[path = "../test/compiled_capabilities.test.rs"]
mod compiled_tests;
