use super::*;
use async_trait::async_trait;
use nora_ai::http::{HttpClientError, HttpResponse, HttpStreamingResponse};
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Mutex,
};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "nora-copilot-sources-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    fn file(&self, relative: &str, value: Value) -> PathBuf {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        path
    }
    fn apps(&self, token: &str) -> PathBuf {
        self.file(
            ".config/github-copilot/apps.json",
            json!({"github.com:Iv1.app":{"user":"octocat","oauth_token":token}}),
        )
    }
    fn cli(&self, token: &str) -> PathBuf {
        self.file(
            ".copilot/config.json",
            json!({
                "lastLoggedInUser":{"host":"https://github.com","login":"octocat"},
                "loggedInUsers":[{"host":"https://github.com","login":"octocat"}],
                "copilotTokens":{"https://github.com:octocat":token}
            }),
        )
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct TokenClient {
    requests: Mutex<Vec<HttpRequest>>,
}
#[async_trait]
impl HttpClient for TokenClient {
    async fn stream(&self, _: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        unreachable!()
    }
    async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request.clone());
        let authorization = request
            .headers
            .get("authorization")
            .cloned()
            .unwrap_or_default();
        let (status, payload) = if authorization.contains("refused") {
            (401, json!({"message":"Bad credentials"}))
        } else if request.url.ends_with("/user") && !request.url.contains("copilot_internal") {
            let changed = authorization.contains("other");
            (
                200,
                json!({"id":if changed {2} else {1},"login":if changed {"hubot"} else {"octocat"}}),
            )
        } else if request.url.ends_with("/copilot_internal/user") {
            (
                200,
                json!({"copilot_plan":"individual","endpoints":{"api":"https://api.individual.githubcopilot.com"}}),
            )
        } else {
            (
                200,
                json!({"token":format!("session-{authorization}"),"expires_at":4_000_000_000_i64,"endpoints":{"api":"https://api.individual.githubcopilot.com"}}),
            )
        };
        Ok(HttpResponse {
            status,
            headers: BTreeMap::new(),
            body: serde_json::to_vec(&payload).unwrap(),
        })
    }
}

#[tokio::test]
async fn editors_sign_in_is_a_verified_reference_without_copied_tokens() {
    let directory = Directory::new();
    let path = directory.apps("fixture-editors");
    let original = std::fs::read(&path).unwrap();
    let client = TokenClient::default();
    let account = take_over(&path, &client).await.unwrap();
    assert!(is_bound(&account));
    assert!(account.access_token.is_empty());
    assert_eq!(account.refresh_token, None);
    assert_eq!(
        account.extras["accountIdentity"].as_value(),
        &json!({"issuer":"https://github.com","subject":"1"})
    );
    assert_eq!(account.extras["name"].as_str(), Some("octocat"));
    assert!(!serde_json::to_string(&account)
        .unwrap()
        .contains("fixture-editors"));
    assert_eq!(std::fs::read(path).unwrap(), original);
}

#[tokio::test]
async fn every_resolve_reads_the_new_editor_token_and_refuses_another_account() {
    let directory = Directory::new();
    let path = directory.apps("fixture-old");
    let client = TokenClient::default();
    let account = take_over(&path, &client).await.unwrap();
    directory.apps("fixture-new");
    let current = resolve(&account, &client).await.unwrap();
    assert_eq!(current.refresh_token.as_deref(), Some("fixture-new"));
    assert!(current.access_token.contains("fixture-new"));
    directory.apps("fixture-other");
    assert_eq!(
        resolve(&account, &client).await.unwrap_err(),
        EditorsError::AccountChanged
    );
    let discovered = discover(&directory.0, &BTreeMap::new(), &client)
        .await
        .unwrap();
    assert_eq!(
        discovered[0].extras["accountIdentity"].as_value()["subject"],
        "2"
    );
    std::fs::remove_file(path).unwrap();
    assert_eq!(
        resolve(&account, &client).await.unwrap_err(),
        EditorsError::NotSignedIn
    );
}

#[tokio::test]
async fn source_discovery_reads_hosts_and_respects_configuration_directories() {
    let directory = Directory::new();
    directory.file(
        "custom/github-copilot/hosts.json",
        json!({"github.com":{"user":"octocat","oauth_token":"fixture-hosts"}}),
    );
    let environment = BTreeMap::from([(
        "XDG_CONFIG_HOME".into(),
        directory.0.join("custom").to_string_lossy().into_owned(),
    )]);
    let client = TokenClient::default();
    let accounts = discover(&directory.0, &environment, &client).await.unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(
        accounts[0].extras["noraAccountSource"].as_value()["authFilePath"],
        directory
            .0
            .join("custom/github-copilot/hosts.json")
            .to_string_lossy()
            .as_ref()
    );
    assert!(accounts[0].access_token.is_empty());
}

#[tokio::test]
async fn editor_and_cli_credentials_for_the_same_user_are_one_discovered_account() {
    let directory = Directory::new();
    directory.apps("fixture-editor");
    directory.cli("fixture-cli");
    let accounts = discover(&directory.0, &BTreeMap::new(), &TokenClient::default())
        .await
        .unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(
        accounts[0].extras["noraAccountSource"].as_value()["origin"],
        "editor"
    );
}

#[tokio::test]
async fn a_refused_editor_token_gives_way_to_the_same_cli_account() {
    let directory = Directory::new();
    directory.apps("fixture-refused");
    directory.cli("fixture-cli");
    let client = TokenClient::default();
    let accounts = discover(&directory.0, &BTreeMap::new(), &client)
        .await
        .unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(
        accounts[0].extras["noraAccountSource"].as_value()["origin"],
        "cli"
    );
    let transient = resolve(&accounts[0], &client).await.unwrap();
    assert_eq!(transient.access_token, "fixture-cli");
    assert_eq!(
        transient.extras["noraCopilotCLI"].as_value(),
        &Value::Bool(true)
    );
    assert!(!client
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.url.contains("/v2/token")
            && request.headers["authorization"].contains("fixture-cli")));
}

#[tokio::test]
async fn failed_verification_and_invalid_files_are_not_reported_as_sign_out() {
    let directory = Directory::new();
    let client = TokenClient::default();
    assert!(discover(&directory.0, &BTreeMap::new(), &client)
        .await
        .unwrap()
        .is_empty());
    let path = directory.apps("fixture-refused");
    assert_eq!(
        discover(&directory.0, &BTreeMap::new(), &client)
            .await
            .unwrap_err(),
        EditorsError::Rejected
    );
    std::fs::write(&path, b"broken-json").unwrap();
    assert_eq!(
        discover(&directory.0, &BTreeMap::new(), &client)
            .await
            .unwrap_err(),
        EditorsError::InvalidFile
    );
}

#[tokio::test]
async fn github_enterprise_cloud_is_a_different_identity_and_other_hosts_are_ignored() {
    let directory = Directory::new();
    let path = directory.file(
        ".config/github-copilot/apps.json",
        json!({
            "github.com:Iv1.app":{"user":"octocat","oauth_token":"fixture-public"},
            "acme.ghe.com:Iv1.app":{"user":"octocat","oauth_token":"fixture-enterprise"},
            "github.example.net:Iv1.app":{"user":"octocat","oauth_token":"fixture-untrusted"}
        }),
    );
    let client = TokenClient::default();
    let accounts = discover(&directory.0, &BTreeMap::new(), &client)
        .await
        .unwrap();
    assert_eq!(accounts.len(), 2);
    assert_eq!(
        accounts[0].extras["accountIdentity"].as_value()["issuer"],
        "https://github.com"
    );
    assert_eq!(
        accounts[1].extras["accountIdentity"].as_value()["issuer"],
        "https://acme.ghe.com"
    );
    assert_eq!(
        accounts[1].extras["accountId"].as_str(),
        Some("octocat@acme.ghe.com")
    );
    assert!(!client
        .requests
        .lock()
        .unwrap()
        .iter()
        .any(|request| request.url.contains("github.example.net")));
    std::fs::remove_file(path).unwrap();
}

#[tokio::test]
async fn cli_home_is_observed_and_bound_reference_contains_no_plaintext_cli_token() {
    let directory = Directory::new();
    directory.file(
        "custom-cli/config.json",
        json!({
            "lastLoggedInUser":{"host":"https://github.com","login":"octocat"},
            "copilotTokens":{"https://github.com:octocat":"fixture-custom-cli"}
        }),
    );
    let environment = BTreeMap::from([(
        "COPILOT_HOME".into(),
        directory
            .0
            .join("custom-cli")
            .to_string_lossy()
            .into_owned(),
    )]);
    let accounts = discover(&directory.0, &environment, &TokenClient::default())
        .await
        .unwrap();
    assert_eq!(accounts.len(), 1);
    assert!(!serde_json::to_string(&accounts)
        .unwrap()
        .contains("fixture-custom-cli"));
}

#[test]
fn relative_environment_directories_do_not_change_the_source_location() {
    let home = Path::new("/fixture-home");
    let environment = BTreeMap::from([
        ("XDG_CONFIG_HOME".into(), "relative".into()),
        ("COPILOT_HOME".into(), "relative".into()),
    ]);
    assert_eq!(
        apps_file(home, &environment),
        home.join(".config/github-copilot/apps.json")
    );
    assert_eq!(
        cli_file(home, &environment),
        home.join(".copilot/config.json")
    );
}

#[tokio::test]
async fn a_bound_editor_account_can_use_a_fresh_cli_token_but_never_another_users() {
    let directory = Directory::new();
    directory.apps("fixture-editor-original");
    directory.cli("fixture-cli-original");
    let client = TokenClient::default();
    let account = discover(&directory.0, &BTreeMap::new(), &client)
        .await
        .unwrap()
        .remove(0);
    directory.apps("fixture-refused");
    let current = resolve_with_force(&account, true, &client).await.unwrap();
    assert_eq!(current.access_token, "fixture-cli-original");
    directory.cli("fixture-other");
    assert!(resolve_with_force(&account, true, &client).await.is_err());
}

#[tokio::test]
async fn unchanged_local_tokens_reuse_verified_sessions_and_force_refresh_discards_them() {
    let directory = Directory::new();
    let path = directory.apps("fixture-cache");
    let client = TokenClient::default();
    let account = take_over(&path, &client).await.unwrap();
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    resolve(&account, &client).await.unwrap();
    resolve(&account, &client).await.unwrap();
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    resolve_with_force(&account, true, &client).await.unwrap();
    assert_eq!(client.requests.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn cli_json_comments_and_trailing_commas_preserve_tokens_that_contain_url_text() {
    let directory = Directory::new();
    let path = directory.file(".copilot/config.json", json!({}));
    std::fs::write(
        path,
        br#"{
        // The CLI permits comments in this file.
        "lastLoggedInUser":{"host":"https://github.com","login":"octocat",},
        "copilotTokens":{"https://github.com:octocat":"fixture-https://token.example/*literal*/",},
    }"#,
    )
    .unwrap();
    let client = TokenClient::default();
    let accounts = discover(&directory.0, &BTreeMap::new(), &client)
        .await
        .unwrap();
    assert_eq!(accounts.len(), 1);
    let current = resolve(&accounts[0], &client).await.unwrap();
    assert_eq!(
        current.access_token,
        "fixture-https://token.example/*literal*/"
    );
}

struct FaultClient {
    path: &'static str,
    status: u16,
}
#[async_trait]
impl HttpClient for FaultClient {
    async fn stream(&self, _: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        unreachable!()
    }
    async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        if request.url == format!("https://api.github.com{}", self.path) {
            if self.status == 0 {
                return Err(HttpClientError::Transport("private-network-detail".into()));
            }
            return Ok(HttpResponse {
                status: self.status,
                headers: BTreeMap::new(),
                body: if self.status == 200 {
                    b"invalid-json".to_vec()
                } else {
                    br#"{"private":"must-not-escape"}"#.to_vec()
                },
            });
        }
        TokenClient::default().request(request).await
    }
}
#[tokio::test]
async fn temporary_failures_at_every_source_endpoint_preserve_credentials_and_retry_in_place() {
    for (path, cli) in [
        ("/user", false),
        ("/copilot_internal/v2/token", false),
        ("/copilot_internal/user", true),
    ] {
        for status in [0, 200, 429, 500, 502, 503, 504] {
            let directory = Directory::new();
            let file = if cli {
                directory.cli("fixture-cli")
            } else {
                directory.apps("fixture-editor")
            };
            let before = std::fs::read(&file).unwrap();
            let client = FaultClient { path, status };
            assert_eq!(
                discover(&directory.0, &BTreeMap::new(), &client)
                    .await
                    .unwrap_err(),
                EditorsError::Unavailable,
                "{path} {status}"
            );
            let account = discover(&directory.0, &BTreeMap::new(), &TokenClient::default())
                .await
                .unwrap()
                .remove(0);
            let saved = account.clone();
            assert_eq!(
                resolve_with_force(&account, true, &client)
                    .await
                    .unwrap_err(),
                EditorsError::Unavailable
            );
            assert_eq!(account, saved);
            assert_eq!(std::fs::read(&file).unwrap(), before);
            assert!(resolve(&account, &TokenClient::default()).await.is_ok());
        }
    }
}
#[tokio::test]
async fn actual_401_and_403_source_refusals_remain_authentication_errors_without_erasing_files() {
    for (path, cli) in [
        ("/user", false),
        ("/copilot_internal/v2/token", false),
        ("/copilot_internal/user", true),
    ] {
        for status in [401, 403] {
            let directory = Directory::new();
            let file = if cli {
                directory.cli("fixture-cli")
            } else {
                directory.apps("fixture-editor")
            };
            let before = std::fs::read(&file).unwrap();
            assert_eq!(
                discover(
                    &directory.0,
                    &BTreeMap::new(),
                    &FaultClient { path, status }
                )
                .await
                .unwrap_err(),
                EditorsError::Rejected
            );
            assert_eq!(std::fs::read(&file).unwrap(), before);
        }
    }
}

struct OneAccountUnavailable;
#[async_trait]
impl HttpClient for OneAccountUnavailable {
    async fn stream(&self, _: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        unreachable!()
    }
    async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        if request
            .headers
            .get("authorization")
            .is_some_and(|value| value.contains("fixture-first"))
        {
            return Err(HttpClientError::Transport("temporary".into()));
        }
        TokenClient::default().request(request).await
    }
}
#[tokio::test]
async fn another_successful_account_does_not_turn_the_selected_accounts_network_failure_into_an_account_change(
) {
    let directory = Directory::new();
    let path = directory.apps("fixture-first");
    let account = take_over(&path, &TokenClient::default()).await.unwrap();
    directory.file(".config/github-copilot/apps.json",json!({"github.com:A":{"user":"octocat","oauth_token":"fixture-first"},"github.com:B":{"user":"hubot","oauth_token":"fixture-other"}}));
    let before = std::fs::read(&path).unwrap();
    assert_eq!(
        resolve_with_force(&account, true, &OneAccountUnavailable)
            .await
            .unwrap_err(),
        EditorsError::Unavailable
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
}
