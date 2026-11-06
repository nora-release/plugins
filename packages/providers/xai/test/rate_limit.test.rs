use super::*;
use nora_ai::{HttpClientError, HttpRequest, HttpStreamingResponse};

struct Directory(std::path::PathBuf);
impl Directory {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!(
            "nora-grok-rate-process-{}",
            nora_ai::oauth_pkce::random_hex(8)
        )))
    }
    fn provider(&self) -> Xai {
        let provider = Xai::default();
        provider.initialize_data_directory(&self.0).unwrap();
        provider
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct Billing;
#[async_trait]
impl HttpClient for Billing {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        assert!(request.url.contains("/billing"));
        Ok(HttpStreamingResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: Box::pin(futures::stream::once(async {
                Ok(
                    serde_json::to_vec(&json!({"config":{"creditUsagePercent":0}}))
                        .unwrap()
                        .into(),
                )
            })),
        })
    }
}
fn credentials(user: &str, organization: &str) -> Value {
    json!({"type":"oauth","credentialProvider":"xAI","access":"fixture","extras":{"userId":user,"organizationId":organization}})
}
fn auth(provider: &Xai, credentials: &Value) -> ResolvedProviderAuth {
    let definition =
        Definition::parse(serde_json::from_str(include_str!("../nora-plugin.json")).unwrap())
            .unwrap();
    let credentials = nora_provider::auth::decode(&definition, Some(credentials)).unwrap();
    let mut auth = nora_provider::auth::base_auth("xai", &credentials);
    provider.enrich_auth(&credentials, &mut auth);
    auth
}

#[tokio::test]
async fn rate_limit_is_visible_for_only_the_affected_billing_account_and_success_clears_it() {
    let directory = Directory::new();
    let provider = directory.provider();
    let first = credentials("person-a", "org-a");
    let other_person = credentials("person-b", "org-a");
    let other_org = credentials("person-a", "org-b");
    provider.observe_response(
        &first,
        429,
        &BTreeMap::from([("Retry-After".into(), "60".into())]),
    );
    let usage = provider
        .usage(&auth(&provider, &first), Some("person-a"), &Billing)
        .await
        .unwrap();
    let rate = usage["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "rate_limit")
        .expect("429 must appear on the account card");
    assert_eq!(rate["usedPercent"], 100.0);
    assert_eq!(rate["countsForRouting"], false);
    assert!(rate["resetsAt"].as_str().is_some());
    for unaffected in [&other_person, &other_org] {
        let usage = provider
            .usage(&auth(&provider, unaffected), Some("person-a"), &Billing)
            .await
            .unwrap();
        assert!(usage["items"]
            .as_array()
            .unwrap()
            .iter()
            .all(|item| item["id"] != "rate_limit"));
    }
    provider.observe_response(&first, 200, &BTreeMap::new());
    let usage = provider
        .usage(&auth(&provider, &first), Some("person-a"), &Billing)
        .await
        .unwrap();
    assert!(usage["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|item| item["id"] != "rate_limit"));
}

#[tokio::test]
async fn usage_and_fetch_adapters_share_the_same_account_hold() {
    let directory = Directory::new();
    let fetch = directory.provider();
    let usage = directory.provider();
    let account = credentials("cross-process-person", "org");
    fetch.observe_response(
        &account,
        429,
        &BTreeMap::from([("retry-after".into(), "60".into())]),
    );
    let result = usage
        .usage(
            &auth(&usage, &account),
            Some("cross-process-person"),
            &Billing,
        )
        .await
        .unwrap();
    assert!(result["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|item| item["id"] == "rate_limit"));
}

#[tokio::test]
async fn process_fixture() {
    let Some(directory) = std::env::var_os("NORA_XAI_RATE_LIMIT_TEST_DIR") else {
        return;
    };
    let provider = Xai::default();
    provider
        .initialize_data_directory(std::path::Path::new(&directory))
        .unwrap();
    let account = credentials("process-person", "org");
    match std::env::var("NORA_XAI_RATE_LIMIT_TEST_ACTION")
        .unwrap()
        .as_str()
    {
        "429" => provider.observe_response(
            &account,
            429,
            &BTreeMap::from([("retry-after".into(), "60".into())]),
        ),
        "clear" => provider.observe_response(&account, 200, &BTreeMap::new()),
        action => {
            let result = provider
                .usage(&auth(&provider, &account), Some("process-person"), &Billing)
                .await
                .unwrap();
            let limited = result["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|item| item["id"] == "rate_limit");
            assert_eq!(limited, action == "usage");
        }
    }
}

#[test]
fn separate_processes_observe_and_clear_a_hold_without_persisting_account_credentials() {
    let directory = Directory::new();
    let fixture = format!(
        "{}::process_fixture",
        module_path!().split_once("::").unwrap().1
    );
    for action in ["429", "usage", "clear", "empty"] {
        let result = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &fixture, "--nocapture"])
            .env("NORA_XAI_RATE_LIMIT_TEST_DIR", &directory.0)
            .env("NORA_XAI_RATE_LIMIT_TEST_ACTION", action)
            .output()
            .unwrap();
        assert!(
            result.status.success(),
            "{action}: {}{}",
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(String::from_utf8_lossy(&result.stdout).contains("1 passed"));
        for entry in std::fs::read_dir(directory.0.join("rate-limits")).unwrap() {
            let entry = entry.unwrap();
            let filename = entry.file_name().to_string_lossy().to_string();
            assert!(!filename.contains("process-person"));
            let bytes = std::fs::read(entry.path()).unwrap();
            assert!(!String::from_utf8_lossy(&bytes).contains("fixture"));
            assert!(!String::from_utf8_lossy(&bytes).contains("process-person"));
            if filename.ends_with(".until") {
                assert!(String::from_utf8_lossy(&bytes).parse::<i64>().is_ok());
            }
        }
    }
}
