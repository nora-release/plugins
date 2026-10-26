use super::*;
use crate::tests::Client;

#[tokio::test]
async fn enterprise_auth_methods_follow_an_isolated_valid_domain_preset() {
    const CHILD: &str = "NORA_QWENWORK_AUTH_METHODS_TEST_CASE";
    let Ok(case) = std::env::var(CHILD) else {
        for (case, preset) in [
            ("absent", None),
            ("valid", Some("https://preset.example.com/")),
            ("invalid", Some("http://localhost")),
        ] {
            let mut command = std::process::Command::new(std::env::current_exe().unwrap());
            command
                .args([
                    "--exact",
                    "qwenwork_auth_methods_tests::enterprise_auth_methods_follow_an_isolated_valid_domain_preset",
                    "--nocapture",
                ])
                .env(CHILD, case)
                .env_remove("QWENWORK_ENT_DOMAIN");
            if let Some(preset) = preset {
                command.env("QWENWORK_ENT_DOMAIN", preset);
            }
            let result = command.output().unwrap();
            assert!(
                result.status.success(),
                "{case}: {}\n{}",
                String::from_utf8_lossy(&result.stdout),
                String::from_utf8_lossy(&result.stderr)
            );
        }
        return;
    };

    for site in [Site::QwenWorkEnterprise, Site::QwenWork, Site::China] {
        let manifest: Value = serde_json::from_str(match site {
            Site::QwenWorkEnterprise => {
                include_str!("../../qwenwork-enterprise/nora-provider.json")
            }
            Site::QwenWork => include_str!("../../qwenwork/nora-provider.json"),
            Site::China => include_str!("../../qoder-cn/nora-provider.json"),
            Site::International => unreachable!(),
        })
        .unwrap();
        let client = Client::new(vec![]);
        let mut provider = Qoder::new(site);
        provider.client = Some(client.clone());
        provider
            .initialize(InitializeParams {
                subscription_accounts_enabled: true,
                installed_manifest: None,
                runtime_config: Value::Null,
                api_version: 1,
                plugin_id: site.id().into(),
                plugin_directory: "/test/plugin".into(),
                data_directory: "/test/data".into(),
                manifest: manifest.clone(),
            })
            .await
            .unwrap();
        let methods = provider.auth_methods(CallContext::default()).await.unwrap();
        if site != Site::QwenWorkEnterprise {
            assert_eq!(methods, manifest["authMethods"]);
            continue;
        }
        if case == "valid" {
            assert_eq!(methods[0]["prompts"], json!([]));
            assert!(text(&methods[0], "label").ends_with("(preset.example.com)"));
            let flow = provider
                .authorize(json!({"authMethodID":"oauth","inputs":{}}))
                .await
                .unwrap();
            assert!(text(&flow, "url").starts_with("https://preset.example.com/"));
            let flow = provider
                .authorize(json!({"authMethodID":"oauth","inputs":{"domain":"other.example.com"}}))
                .await
                .unwrap();
            assert!(text(&flow, "url").starts_with("https://preset.example.com/"));
        } else {
            assert_eq!(methods, manifest["authMethods"]);
            assert_eq!(methods[0]["prompts"][0]["required"], true);
            assert!(provider
                .authorize(json!({"authMethodID":"oauth","inputs":{}}))
                .await
                .is_err());
            let flow = provider
                .authorize(json!({"authMethodID":"oauth","inputs":{"domain":"typed.example.com"}}))
                .await
                .unwrap();
            assert!(text(&flow, "url").starts_with("https://typed.example.com/"));
        }
        assert!(client.requests.lock().unwrap().is_empty());
    }
}
