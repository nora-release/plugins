use super::*;
use nora_ai::http::{HttpRequest, HttpResponse, HttpStreamingResponse};
use nora_ai::model::{InputModality, Model, ModelCost};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

#[derive(Default)]
struct NoopHttpClient;

#[async_trait]
impl HttpClient for NoopHttpClient {
    async fn stream(
        &self,
        _request: HttpRequest,
    ) -> Result<HttpStreamingResponse, HttpClientError> {
        unreachable!("OAuth tests use provider stubs")
    }

    async fn request(&self, _request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        unreachable!("OAuth tests use provider stubs")
    }
}

struct StaticOAuthProvider {
    id: String,
    refreshed: OAuthCredentials,
    refresh_count: Arc<AtomicUsize>,
}

#[async_trait]
impl OAuthProvider for StaticOAuthProvider {
    fn id(&self) -> &str {
        &self.id
    }

    fn name(&self) -> &str {
        "Static"
    }

    async fn refresh(
        &self,
        _credentials: &OAuthCredentials,
        _client: &dyn HttpClient,
    ) -> Result<OAuthCredentials, OAuthError> {
        self.refresh_count.fetch_add(1, Ordering::SeqCst);
        Ok(self.refreshed.clone())
    }
}

#[test]
fn expiration_checks_optional_expiry() {
    let never = OAuthCredentials::api_key(OAuthProviderId::ApiKey, "key");
    assert!(!never.is_expired_at(1_000));

    let expired = OAuthCredentials::new(
        OAuthProviderId::Anthropic,
        "access",
        Some("refresh".to_string()),
        Some(999),
        BTreeMap::new(),
    );
    assert!(expired.is_expired_at(1_000));
}

#[test]
fn url_encoded_form_sorts_and_escapes_values() {
    let form = url_encoded_form(&BTreeMap::from([
        ("urn".to_string(), "urn:ietf:params:oauth".to_string()),
        ("space".to_string(), "a b".to_string()),
    ]));

    assert_eq!(form, "space=a%20b&urn=urn%3Aietf%3Aparams%3Aoauth");
}

#[test]
fn store_persists_sets_exclusive_and_removes_credentials() {
    let path = temp_oauth_path("store");
    let mut store = OAuthStore::new(&path);
    let anthropic = OAuthCredentials::new(
        OAuthProviderId::Anthropic,
        "access",
        Some("refresh".to_string()),
        Some(123),
        BTreeMap::new(),
    );

    store.set("anthropic", anthropic.clone()).unwrap();
    let reloaded = OAuthStore::new(&path);
    assert_eq!(reloaded.get("anthropic"), Some(anthropic.clone()));
    let stored_json = fs::read_to_string(&path).unwrap();
    assert!(stored_json.contains("\"access\""));
    assert!(stored_json.contains("\"refresh\""));
    assert!(stored_json.contains("\"expires\""));
    assert!(!stored_json.contains("accessToken"));
    assert!(!stored_json.contains("\"provider\""));

    let github = OAuthCredentials::new(
        OAuthProviderId::GitHubCopilot,
        "gh-access",
        Some("gh-refresh".to_string()),
        Some(456),
        BTreeMap::new(),
    );
    store
        .set_exclusive("github-copilot", github.clone())
        .unwrap();
    assert_eq!(store.all().len(), 1);
    assert_eq!(OAuthStore::new(&path).get("github-copilot"), Some(github));

    store.remove("github-copilot").unwrap();
    assert!(OAuthStore::new(&path).all().is_empty());

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        let directory_mode = fs::metadata(path.parent().unwrap())
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(directory_mode, 0o700);
    }
    let temporary_files = fs::read_dir(path.parent().unwrap())
        .unwrap()
        .filter_map(Result::ok)
        .filter(|entry| entry.file_name().to_string_lossy().contains(".tmp-"))
        .count();
    assert_eq!(temporary_files, 0);

    cleanup_temp_oauth_path(&path);
}

#[test]
fn store_reads_nora_swift_schema_without_provider_field() {
    let path = temp_oauth_path("swift-schema");
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
        &path,
        r#"{"anthropic":{"access":"access","refresh":"refresh","expires":999,"extras":{"x":"y"}}}"#,
    )
    .unwrap();

    let credential = OAuthStore::new(&path).get("anthropic").unwrap();

    assert_eq!(credential.provider, OAuthProviderId::Anthropic);
    assert_eq!(credential.access_token, "access");
    assert_eq!(credential.refresh_token.as_deref(), Some("refresh"));
    assert_eq!(credential.expires_at, Some(999));
    assert_eq!(
        credential.extras.get("x").and_then(|value| value.as_str()),
        Some("y")
    );

    cleanup_temp_oauth_path(&path);
}

#[tokio::test]
async fn manager_refreshes_expired_credentials_and_persists_update() {
    let path = temp_oauth_path("manager-refresh");
    let mut store = OAuthStore::new(&path);
    store
        .set(
            "anthropic",
            OAuthCredentials::new(
                OAuthProviderId::Anthropic,
                "old",
                Some("refresh".to_string()),
                Some(0),
                BTreeMap::new(),
            ),
        )
        .unwrap();
    let refresh_count = Arc::new(AtomicUsize::new(0));
    let refreshed = OAuthCredentials::new(
        OAuthProviderId::Anthropic,
        "new",
        Some("refresh".to_string()),
        Some(i64::MAX),
        BTreeMap::new(),
    );
    let mut manager = OAuthManager::new(
        store,
        vec![Box::new(StaticOAuthProvider {
            id: "anthropic".to_string(),
            refreshed: refreshed.clone(),
            refresh_count: Arc::clone(&refresh_count),
        })],
        Arc::new(NoopHttpClient),
    );

    let api_key = manager.api_key("anthropic").await.unwrap();

    assert_eq!(api_key, "new");
    assert_eq!(refresh_count.load(Ordering::SeqCst), 1);
    assert_eq!(OAuthStore::new(&path).get("anthropic"), Some(refreshed));

    cleanup_temp_oauth_path(&path);
}

#[tokio::test]
async fn manager_reuses_fresh_credentials_without_refreshing() {
    let path = temp_oauth_path("manager-fresh");
    let mut store = OAuthStore::new(&path);
    store
        .set(
            "anthropic",
            OAuthCredentials::new(
                OAuthProviderId::Anthropic,
                "fresh",
                Some("refresh".to_string()),
                Some(i64::MAX),
                BTreeMap::new(),
            ),
        )
        .unwrap();
    let refresh_count = Arc::new(AtomicUsize::new(0));
    let mut manager = OAuthManager::new(
        store,
        vec![Box::new(StaticOAuthProvider {
            id: "anthropic".to_string(),
            refreshed: OAuthCredentials::api_key(OAuthProviderId::Anthropic, "unused"),
            refresh_count: Arc::clone(&refresh_count),
        })],
        Arc::new(NoopHttpClient),
    );

    let api_key = manager.api_key("anthropic").await.unwrap();

    assert_eq!(api_key, "fresh");
    assert_eq!(refresh_count.load(Ordering::SeqCst), 0);

    cleanup_temp_oauth_path(&path);
}

#[tokio::test]
async fn manager_builds_resolved_bearer_auth_and_github_base_url() {
    let path = temp_oauth_path("resolved-auth");
    let mut store = OAuthStore::new(&path);
    store
        .set(
            "github-copilot",
            OAuthCredentials::new(
                OAuthProviderId::GitHubCopilot,
                "token",
                Some("refresh".to_string()),
                Some(i64::MAX),
                BTreeMap::from([(
                    "endpoint".to_string(),
                    JsonValue::string("https://copilot.test"),
                )]),
            ),
        )
        .unwrap();
    let mut manager = OAuthManager::new(
        store,
        vec![Box::new(StaticOAuthProvider {
            id: "github-copilot".to_string(),
            refreshed: OAuthCredentials::api_key(OAuthProviderId::GitHubCopilot, "unused"),
            refresh_count: Arc::new(AtomicUsize::new(0)),
        })],
        Arc::new(NoopHttpClient),
    );

    let auth = manager
        .resolved_auth_for_model(&Model {
            id: "m".to_string(),
            name: "M".to_string(),
            api: "openai-responses".to_string(),
            provider: "github-copilot".to_string(),
            base_url: String::new(),
            reasoning: false,
            input: vec![InputModality::Text],
            cost: ModelCost::default(),
            context_window: 1,
            max_tokens: 1,
            info: Default::default(),
            provider_options: Default::default(),
            headers: None,
            env_http_headers: Default::default(),
            query_params: Default::default(),
        })
        .await
        .unwrap();

    assert_eq!(auth.token.as_deref(), Some("token"));
    assert_eq!(auth.scheme, AuthScheme::Bearer);
    assert_eq!(auth.base_url.as_deref(), Some("https://copilot.test"));

    cleanup_temp_oauth_path(&path);
}

#[tokio::test]
async fn manager_routes_xai_oauth_to_session_proxy_with_session_headers() {
    let path = temp_oauth_path("xai-resolved-auth");
    let mut store = OAuthStore::new(&path);
    store
        .set(
            "xai",
            OAuthCredentials::new(
                OAuthProviderId::XAI,
                "oauth-token",
                Some("refresh".to_string()),
                Some(i64::MAX),
                BTreeMap::from([(
                    "baseUrl".to_string(),
                    JsonValue::string(XAI_API_BASE_URL),
                )]),
            ),
        )
        .unwrap();
    let mut manager = OAuthManager::new(
        store,
        vec![Box::new(StaticOAuthProvider {
            id: "xai".to_string(),
            refreshed: OAuthCredentials::api_key(OAuthProviderId::XAI, "unused"),
            refresh_count: Arc::new(AtomicUsize::new(0)),
        })],
        Arc::new(NoopHttpClient),
    );

    let auth = manager.resolved_auth("xai").await.unwrap();

    assert_eq!(auth.token.as_deref(), Some("oauth-token"));
    assert_eq!(auth.scheme, AuthScheme::Bearer);
    assert_eq!(auth.base_url.as_deref(), Some(XAI_OAUTH_PROXY_BASE_URL));
    assert_eq!(
        auth.headers.get(XAI_TOKEN_AUTH_HEADER).map(String::as_str),
        Some(XAI_TOKEN_AUTH_VALUE)
    );
    assert_eq!(
        auth.headers
            .get(XAI_AUTHENTICATE_RESPONSE_HEADER)
            .map(String::as_str),
        Some(XAI_AUTHENTICATE_RESPONSE_VALUE)
    );

    cleanup_temp_oauth_path(&path);
}

#[tokio::test]
async fn manager_reports_unknown_and_missing_credentials() {
    let mut manager = OAuthManager::new(
        OAuthStore::new(temp_oauth_path("missing")),
        Vec::new(),
        Arc::new(NoopHttpClient),
    );

    assert_eq!(
        manager.api_key("anthropic").await.unwrap_err(),
        OAuthError::UnknownProvider("anthropic".to_string())
    );

    manager.register(Box::new(StaticOAuthProvider {
        id: "anthropic".to_string(),
        refreshed: OAuthCredentials::api_key(OAuthProviderId::Anthropic, "unused"),
        refresh_count: Arc::new(AtomicUsize::new(0)),
    }));

    assert_eq!(
        manager.api_key("anthropic").await.unwrap_err(),
        OAuthError::Missing("anthropic".to_string())
    );
}

fn temp_oauth_path(name: &str) -> PathBuf {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    std::env::temp_dir()
        .join(format!(
            "nora-oauth-{name}-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ))
        .join("oauth.json")
}

fn cleanup_temp_oauth_path(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir_all(parent);
    }
}
