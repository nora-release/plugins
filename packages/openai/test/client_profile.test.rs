use nora_provider_plugin_sdk::{InitializeParams, ProviderPlugin};
use serde_json::json;

#[test]
fn codex_test_profile_is_public_metadata_without_an_account() {
    let path = std::env::temp_dir().join(format!("nora-codex-profile-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&path).unwrap();
    let name = format!(
        "{}::profile_worker",
        module_path!().split_once("::").unwrap().1
    );
    let output = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", &name, "--ignored", "--nocapture"])
        .env("NORA_TEST_PROFILE_ROOT", &path)
        .env("HOME", &path)
        .env("CODEX_HOME", &path)
        .env("PATH", "/usr/bin:/bin")
        .output()
        .unwrap();
    let _ = std::fs::remove_dir_all(path);
    assert!(
        output.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[tokio::test]
#[ignore = "isolated by codex_test_profile_is_public_metadata_without_an_account"]
async fn profile_worker() {
    let path = std::path::PathBuf::from(std::env::var_os("NORA_TEST_PROFILE_ROOT").unwrap());
    let plugin = crate::plugin::OpenAIPlugin::default();
    let result = plugin
        .initialize(InitializeParams {
            api_version: 1,
            subscription_accounts_enabled: false,
            plugin_id: "openai".into(),
            plugin_directory: path.clone(),
            data_directory: path,
            manifest: serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
            installed_manifest: None,
            runtime_config: json!({}),
        })
        .await
        .unwrap();
    assert_eq!(result["capabilities"]["clientProfiles"], json!(["codex"]));
    crate::provider::codex_version::observe(Some("codex_cli_rs/99.1.0"), None, None);
    let result = plugin
        .call("modelMetadata", json!({"clientProfile":"codex"}))
        .await
        .unwrap();
    let profile = &result["clientProfiles"]["codex"];
    assert_eq!(profile["version"], "99.1.0");
    assert_eq!(
        profile,
        &serde_json::to_value(crate::provider::codex_version::headers("99.1.0")).unwrap()
    );
    assert!(plugin
        .call("modelMetadata", json!({"clientProfile":"claude-code"}))
        .await
        .is_err());
}
