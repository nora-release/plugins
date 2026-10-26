use super::*;
use nora_ai::{HttpClientError, HttpStreamingResponse};
use std::collections::VecDeque;
use std::path::PathBuf;
use std::sync::Mutex as SyncMutex;

#[derive(Clone)]
enum Reply {
    Http(u16, Value),
    Raw(u16, Vec<u8>),
    Network,
    Interrupted,
}

struct Client {
    replies: SyncMutex<VecDeque<Reply>>,
    requests: SyncMutex<Vec<HttpRequest>>,
}

impl Client {
    fn new(replies: Vec<Reply>) -> Self {
        Self {
            replies: SyncMutex::new(replies.into()),
            requests: SyncMutex::new(Vec::new()),
        }
    }

    fn requests(&self) -> Vec<HttpRequest> {
        self.requests.lock().unwrap().clone()
    }
}

#[async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        if request.url == VERSION {
            return Ok(HttpStreamingResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: Box::pin(futures::stream::iter([Ok(b"version: 3.0.1\n"
                    .to_vec()
                    .into())])),
            });
        }
        self.requests.lock().unwrap().push(request);
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected request");
        let (status, body) = match reply {
            Reply::Http(status, value) => (status, Ok(value.to_string().into_bytes().into())),
            Reply::Raw(status, bytes) => (status, Ok(bytes.into())),
            Reply::Network => {
                return Err(HttpClientError::Transport("private-network-detail".into()))
            }
            Reply::Interrupted => (
                200,
                Err(HttpClientError::Transport("private-body-detail".into())),
            ),
        };
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::new(),
            body: Box::pin(futures::stream::iter([body])),
        })
    }
}

async fn provider(app: App, replies: Vec<Reply>) -> (CodeAssist, Arc<Client>) {
    let client = Arc::new(Client::new(replies));
    let mut provider = CodeAssist::new(app);
    provider.client = Some(client.clone());
    let manifest = serde_json::from_str(match app {
        App::GeminiCLI => include_str!("../../gemini-cli/nora-provider.json"),
        App::Antigravity => include_str!("../../antigravity/nora-provider.json"),
    })
    .unwrap();
    provider
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: Value::Null,
            api_version: API_VERSION,
            plugin_id: app.id().into(),
            plugin_directory: Default::default(),
            data_directory: Default::default(),
            manifest,
        })
        .await
        .unwrap();
    (provider, client)
}

#[derive(Clone, Copy, Debug)]
enum Kind {
    Owned,
    Selected,
    Gemini,
}

struct Account {
    directory: PathBuf,
    kind: Kind,
    app: App,
    saved: Value,
}

impl Account {
    fn new(app: App, kind: Kind) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "nora-codeassist-refresh-{}",
            nora_ai::oauth_pkce::random_hex(12)
        ));
        std::fs::create_dir(&directory).unwrap();
        let tokens = session::Tokens {
            access: "private-old-access".into(),
            refresh: "private-old-refresh".into(),
            expires: 1,
            uid: fingerprint("google-account\0person-a"),
            email: "person@example.test".into(),
            identity: Some(json!({"issuer":"https://accounts.google.com","subject":"person-a"})),
            ..Default::default()
        };
        let path = directory.join("oauth_creds.json");
        let saved = match kind {
            Kind::Owned => session::saved(&tokens, app, None),
            Kind::Selected => session::saved(&tokens, app, Some(path.to_str().unwrap())),
            Kind::Gemini => gemini_source::saved(&tokens, &path).unwrap(),
        };
        let value = Self {
            directory,
            kind,
            app,
            saved,
        };
        value.write("private-old-access", "private-old-refresh", 1);
        value
    }

    fn path(&self) -> PathBuf {
        self.directory.join("oauth_creds.json")
    }

    fn write(&self, access: &str, refresh: &str, expires: i64) {
        std::fs::write(self.path(), json!({"type":self.app.id(),"client_id":self.app.client_id(),"access_token":access,"refresh_token":refresh,"expiry_date":expires,"retained":"keep"}).to_string()).unwrap();
    }

    fn login_again(&mut self) {
        if matches!(self.kind, Kind::Owned) {
            self.saved["access"] = json!("private-new-login-access");
            self.saved["refresh"] = json!("private-new-login-refresh");
            self.saved["expires"] = json!(now() + 3_600_000);
        } else {
            self.write(
                "private-new-login-access",
                "private-new-login-refresh",
                now() + 3_600_000,
            );
        }
    }
}

impl Drop for Account {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn kinds(app: App) -> Vec<Kind> {
    match app {
        App::GeminiCLI => vec![Kind::Owned, Kind::Selected, Kind::Gemini],
        App::Antigravity => vec![Kind::Owned, Kind::Selected],
    }
}

fn identity() -> Reply {
    Reply::Http(
        200,
        json!({"id":"person-a","email":"person@example.test","verified_email":true}),
    )
}

fn renewed() -> Reply {
    Reply::Http(
        200,
        json!({"access_token":"private-renewed-access","refresh_token":"private-renewed-refresh","expires_in":3600}),
    )
}

fn form(request: &HttpRequest) -> BTreeMap<String, String> {
    url::form_urlencoded::parse(request.body.as_ref().unwrap())
        .into_owned()
        .collect()
}

#[tokio::test]
async fn permanent_refresh_refusals_reach_owned_and_source_accounts() {
    for app in [App::GeminiCLI, App::Antigravity] {
        for kind in kinds(app) {
            for reply in [
                Reply::Http(
                    400,
                    json!({"error":"invalid_grant","error_description":"private-已撤销"}),
                ),
                Reply::Http(
                    400,
                    json!({"error":"invalid_token","error_description":"private-Bad Request"}),
                ),
                Reply::Http(401, json!({"error":"invalid_grant"})),
                Reply::Raw(401, b"private-body".to_vec()),
            ] {
                let account = Account::new(app, kind);
                let original = std::fs::read(account.path()).unwrap();
                let (provider, client) = provider(app, vec![reply]).await;
                let error = provider
                    .refresh(json!({"credentials":account.saved}))
                    .await
                    .unwrap_err();
                assert_eq!(
                    error.code, "authentication_required",
                    "{app:?} {kind:?}: {error:?}"
                );
                assert!(!error.message.contains("private-"));
                assert_eq!(std::fs::read(account.path()).unwrap(), original);
                assert!(provider.tokens.lock().await.is_empty());
                let requests = client.requests();
                assert_eq!(requests.len(), 1);
                assert_eq!(requests[0].url, TOKEN);
                assert_eq!(form(&requests[0])["client_id"], app.client_id());
                assert_eq!(form(&requests[0])["refresh_token"], "private-old-refresh");
            }
        }
    }
}

#[tokio::test]
async fn temporary_refresh_failures_do_not_lapse_owned_or_source_accounts() {
    let mut replies = vec![
        Reply::Http(
            400,
            json!({"error":"invalid_request","error_description":"invalid_grant private-token was revoked"}),
        ),
        Reply::Raw(400, b"private-invalid_grant".to_vec()),
        Reply::Http(400, json!({"error":"invalid_client"})),
        Reply::Http(
            400,
            json!({"error":"invalid_grant","padding":"x".repeat(64 * 1024)}),
        ),
        Reply::Network,
        Reply::Interrupted,
        Reply::Raw(200, b"private-invalid-json".to_vec()),
    ];
    for status in [403, 408, 429, 500, 502, 503] {
        replies.push(Reply::Http(
            status,
            json!({"error":"invalid_grant","error_description":"private-revoked"}),
        ));
    }
    for app in [App::GeminiCLI, App::Antigravity] {
        for kind in kinds(app) {
            for reply in &replies {
                let account = Account::new(app, kind);
                let original = std::fs::read(account.path()).unwrap();
                let (provider, client) = provider(app, vec![reply.clone()]).await;
                let error = provider
                    .refresh(json!({"credentials":account.saved}))
                    .await
                    .unwrap_err();
                assert_eq!(
                    error.code, "upstream_transport_error",
                    "{app:?} {kind:?}: {error:?}"
                );
                assert!(!error.message.contains("private-"));
                assert_eq!(std::fs::read(account.path()).unwrap(), original);
                assert!(provider.tokens.lock().await.is_empty());
                assert_eq!(client.requests().len(), 1);
            }
        }
    }
}

#[tokio::test]
async fn incomplete_successful_refresh_is_temporary_and_keeps_previous_credentials() {
    for app in [App::GeminiCLI, App::Antigravity] {
        for kind in kinds(app) {
            for payload in [
                json!({}),
                json!({"access_token":"private-access","expires_in":0}),
                json!({"access_token":"private-access","expires_in":3600,"refresh_token":" "}),
                json!({"access_token":"private invalid access","expires_in":3600}),
            ] {
                let account = Account::new(app, kind);
                let original = std::fs::read(account.path()).unwrap();
                let (provider, _) = provider(app, vec![Reply::Http(200, payload)]).await;
                let error = provider
                    .refresh(json!({"credentials":account.saved}))
                    .await
                    .unwrap_err();
                assert_eq!(
                    error.code, "upstream_transport_error",
                    "{app:?} {kind:?}: {error:?}"
                );
                assert!(!error.message.contains("private-"));
                assert_eq!(std::fs::read(account.path()).unwrap(), original);
                assert!(provider.tokens.lock().await.is_empty());
            }
        }
    }
}

#[tokio::test]
async fn a_new_login_recovers_after_permanent_refusal_without_rebinding_its_identity() {
    for app in [App::GeminiCLI, App::Antigravity] {
        for kind in kinds(app) {
            let mut replies = vec![Reply::Http(400, json!({"error":"invalid_grant"}))];
            if !matches!(kind, Kind::Owned) {
                replies.push(identity());
            }
            let mut account = Account::new(app, kind);
            let bound_identity = account.saved["accountIdentity"].clone();
            let (provider, client) = provider(app, replies).await;
            assert_eq!(
                provider
                    .refresh(json!({"credentials":account.saved}))
                    .await
                    .unwrap_err()
                    .code,
                "authentication_required"
            );
            account.login_again();
            let original = std::fs::read(account.path()).unwrap();
            let recovered = provider
                .refresh(json!({"credentials":account.saved}))
                .await
                .unwrap();
            if matches!(kind, Kind::Owned) {
                assert_eq!(
                    recovered["credentials"]["refresh"],
                    "private-new-login-refresh"
                );
                assert_eq!(recovered["credentials"]["accountIdentity"], bound_identity);
            } else {
                assert_eq!(recovered, json!({"changed":false}));
                assert_eq!(std::fs::read(account.path()).unwrap(), original);
            }
            let requests = client.requests();
            assert_eq!(
                requests
                    .iter()
                    .filter(|request| request.url == TOKEN)
                    .count(),
                1
            );
            if !matches!(kind, Kind::Owned) {
                assert_eq!(
                    requests[1].headers["authorization"],
                    "Bearer private-new-login-access"
                );
            }
        }
    }
}

#[tokio::test]
async fn temporary_refresh_can_retry_the_same_login_and_persist_a_real_rotation() {
    for app in [App::GeminiCLI, App::Antigravity] {
        for kind in kinds(app) {
            let mut replies = vec![
                Reply::Http(503, json!({"error":"temporarily_unavailable"})),
                renewed(),
            ];
            if !matches!(kind, Kind::Owned) {
                replies.push(identity());
            }
            let account = Account::new(app, kind);
            let original = std::fs::read(account.path()).unwrap();
            let (provider, client) = provider(app, replies).await;
            assert_eq!(
                provider
                    .refresh(json!({"credentials":account.saved}))
                    .await
                    .unwrap_err()
                    .code,
                "upstream_transport_error"
            );
            let recovered = provider
                .refresh(json!({"credentials":account.saved}))
                .await
                .unwrap();
            match kind {
                Kind::Owned => assert_eq!(
                    recovered["credentials"]["refresh"],
                    "private-renewed-refresh"
                ),
                Kind::Selected => assert_eq!(std::fs::read(account.path()).unwrap(), original),
                Kind::Gemini => {
                    let saved: Value =
                        serde_json::from_slice(&std::fs::read(account.path()).unwrap()).unwrap();
                    assert_eq!(saved["refresh_token"], "private-renewed-refresh");
                    assert_eq!(saved["retained"], "keep");
                }
            }
            let requests = client.requests();
            assert_eq!(form(&requests[0])["refresh_token"], "private-old-refresh");
            assert_eq!(form(&requests[1])["refresh_token"], "private-old-refresh");
        }
    }
}

#[tokio::test]
async fn concurrent_owned_refreshes_share_a_successful_rotation_and_valid_tokens_need_no_refresh() {
    for app in [App::GeminiCLI, App::Antigravity] {
        let mut account = Account::new(app, Kind::Owned);
        let (provider, client) = provider(app, vec![renewed()]).await;
        let params = json!({"credentials":account.saved});
        let (first, second) =
            tokio::join!(provider.refresh(params.clone()), provider.refresh(params));
        let first = first.unwrap();
        assert_eq!(first, second.unwrap());
        assert_eq!(first["credentials"]["refresh"], "private-renewed-refresh");
        assert_eq!(client.requests().len(), 1);
        account.login_again();
        assert_eq!(
            provider
                .refresh(json!({"credentials":account.saved}))
                .await
                .unwrap()["credentials"]["access"],
            "private-new-login-access"
        );
        assert_eq!(client.requests().len(), 1);
    }
}

#[tokio::test]
async fn a_forced_temporary_failure_preserves_the_existing_valid_token() {
    for app in [App::GeminiCLI, App::Antigravity] {
        for kind in kinds(app) {
            let mut account = Account::new(app, kind);
            account.login_again();
            let mut replies = Vec::new();
            if !matches!(kind, Kind::Owned) {
                replies.push(identity());
            }
            replies.push(Reply::Http(503, json!({"error":"temporarily_unavailable"})));
            let (provider, client) = provider(app, replies).await;
            let current = provider
                .live(Some(&account.saved), client.as_ref(), false)
                .await
                .unwrap();
            let original = std::fs::read(account.path()).unwrap();
            let error = provider
                .refresh(json!({"credentials":account.saved,"force":true}))
                .await
                .unwrap_err();
            assert_eq!(error.code, "upstream_transport_error");
            let retained = provider
                .live(Some(&account.saved), client.as_ref(), false)
                .await
                .unwrap();
            assert_eq!(retained.access, current.access);
            assert_eq!(retained.refresh, current.refresh);
            assert_eq!(retained.uid, current.uid);
            assert_eq!(std::fs::read(account.path()).unwrap(), original);
            assert_eq!(
                client
                    .requests()
                    .iter()
                    .filter(|request| request.url == TOKEN)
                    .count(),
                1
            );
        }
    }
}

#[tokio::test]
async fn oauth_refusal_codes_do_not_change_code_assist_request_error_classification() {
    let client = Client::new(vec![Reply::Http(400, json!({"error":"invalid_grant"}))]);
    let error = auth::json_request(
        &client,
        HttpRequest::new(
            HttpMethod::Post,
            format!("{PROD}/v1internal:loadCodeAssist"),
        ),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, "upstream_transport_error");
}
