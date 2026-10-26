use std::path::PathBuf;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path =
            std::env::temp_dir().join(format!("nora-codex-dispatch-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn actual_plugin_fetch_dispatch_teaches_new_metadata_workers_the_caller_version() {
    let directory = Directory::new();
    for mode in ["fetch", "metadata"] {
        let worker_name = format!(
            "{}::plugin_dispatch_worker",
            module_path!().split_once("::").unwrap().1
        );
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &worker_name, "--ignored", "--nocapture"])
            .env("NORA_CODEX_TEST_DIRECTORY", &directory.0)
            .env("NORA_CODEX_TEST_MODE", mode)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains("running 1 test") && stdout.contains("1 passed; 0 failed"),
            "{stdout}"
        );
    }
}

#[tokio::test]
#[ignore = "launched by actual_plugin_fetch_dispatch_teaches_new_metadata_workers_the_caller_version"]
async fn plugin_dispatch_worker() {
    use nora_provider::ProviderAdapter;
    use nora_provider_plugin_sdk::{FetchRequest, InitializeParams, ProviderPlugin};
    use serde_json::json;
    let directory = PathBuf::from(std::env::var_os("NORA_CODEX_TEST_DIRECTORY").unwrap());
    let plugin = crate::plugin::OpenAIPlugin::default();
    plugin
        .initialize(InitializeParams {
            api_version: 1,
            subscription_accounts_enabled: true,
            plugin_id: "openai".into(),
            plugin_directory: directory.clone(),
            data_directory: directory.clone(),
            manifest: serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
            installed_manifest: None,
            runtime_config: serde_json::Value::Null,
        })
        .await
        .unwrap();
    let credentials = json!({"type":"oauth","provider":"openAICodex","credentialProvider":"openAICodex","access":"fixture-chatgpt-access","expires":nora_ai::oauth::now_millis()+3600000,"extras":{"accountId":"fixture-workspace"}});
    if std::env::var("NORA_CODEX_TEST_MODE").unwrap() == "fetch" {
        // The request is refused before HTTP; the production wrapper still
        // receives the same caller metadata used for a valid proxy request.
        let mut request: FetchRequest = serde_json::from_value(json!({"url":"not a URL","method":"POST","account":"fixture","credentials":credentials,"callerUserAgent":"Codex Desktop/99.160.0","metadata":{"callerVersion":"99.162.3","callerOriginator":"codex_desktop"},"body":null})).unwrap();
        assert!(plugin.fetch(request.clone()).await.is_err());
        assert_eq!(
            std::fs::read_to_string(directory.join("codex-client-version"))
                .ok()
                .as_deref(),
            Some("99.162.3")
        );
        request.credentials = Some(json!({"type":"apiKey","key":"fixture-api-key"}));
        request
            .metadata
            .insert("callerVersion".into(), "100.0.0".into());
        assert!(plugin.fetch(request.clone()).await.is_err());
        request.credentials = Some(credentials);
        request.subscription_accounts_enabled = false;
        assert!(plugin.fetch(request).await.is_err());
        assert_eq!(
            std::fs::read_to_string(directory.join("codex-client-version"))
                .ok()
                .as_deref(),
            Some("99.162.3")
        );
    } else {
        let version = crate::provider::codex_version::current().await;
        assert_eq!(version, "99.162.3");
        let models = crate::provider::OpenAI
            .model_urls("https://chatgpt.com/backend-api/codex", &credentials)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            models[0]
                .query_pairs()
                .find(|(key, _)| key == "client_version")
                .unwrap()
                .1,
            "99.162.3"
        );
        let mut auth = nora_ai::ResolvedProviderAuth::default();
        let definition = nora_provider::definition::Definition::parse(
            serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
        )
        .unwrap();
        crate::provider::OpenAI
            .prepare_fetch(
                &definition,
                &credentials,
                &mut url::Url::parse("https://chatgpt.com/backend-api/codex/responses").unwrap(),
                &mut Default::default(),
                &mut None,
                &mut auth,
            )
            .await
            .unwrap();
        assert_eq!(auth.headers["version"], "99.162.3");
        assert!(auth.headers["user-agent"].starts_with("codex_cli_rs/99.162.3 "));
    }
}
