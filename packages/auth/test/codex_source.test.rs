use super::*;
use async_trait::async_trait;
use base64::Engine;
use nora_ai::http::{HttpClientError, HttpRequest, HttpResponse, HttpStreamingResponse};
use serde_json::json;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Directory(PathBuf);

impl Directory {
    fn new() -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let path = std::env::temp_dir().join(format!(
            "nora-codex-source-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn auth_file(&self, value: Value) -> PathBuf {
        let path = self.0.join("auth.json");
        std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
        path
    }
}

impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct RenewingClient {
    requests: AtomicUsize,
    body: Value,
}

#[async_trait]
impl HttpClient for RenewingClient {
    async fn stream(
        &self,
        _request: HttpRequest,
    ) -> Result<HttpStreamingResponse, HttpClientError> {
        unreachable!("renewal uses one-shot requests")
    }

    async fn request(&self, _request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        self.requests.fetch_add(1, Ordering::Relaxed);
        Ok(HttpResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: serde_json::to_vec(&self.body).unwrap(),
        })
    }
}

fn client(body: Value) -> RenewingClient {
    RenewingClient {
        requests: AtomicUsize::new(0),
        body,
    }
}

fn jwt(claims: Value) -> String {
    let encode = |value: &Value| {
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(serde_json::to_vec(value).unwrap())
    };
    format!(
        "{}.{}.signature",
        encode(&json!({"alg":"none"})),
        encode(&claims)
    )
}

fn access(account: &str, expires_in_seconds: i64) -> String {
    jwt(json!({
        "exp": now_millis() / 1000 + expires_in_seconds,
        "https://api.openai.com/auth": {"chatgpt_account_id": account, "chatgpt_plan_type": "pro"},
        "https://api.openai.com/profile": {"email": "person@example.com"}
    }))
}

fn codex_file(access_token: &str, account: &str) -> Value {
    json!({
        "OPENAI_API_KEY": null,
        "auth_mode": "chatgpt",
        "last_refresh": "2026-01-01T00:00:00Z",
        "tokens": {
            "access_token": access_token,
            "refresh_token": "refresh-1",
            "id_token": "id-1",
            "account_id": account
        }
    })
}

#[test]
fn binding_saves_where_and_whose_the_sign_in_is_but_no_token() {
    let directory = Directory::new();
    let path = directory.auth_file(codex_file(&access("account-1", 3600), "account-1"));

    let credentials = bind(&path).unwrap();

    assert!(is_bound(&credentials));
    assert!(credentials.access_token.is_empty());
    assert_eq!(credentials.refresh_token, None);
    assert_eq!(credentials.extras["accountId"].as_str(), Some("account-1"));
    assert_eq!(
        credentials.extras["email"].as_str(),
        Some("person@example.com")
    );
    assert_eq!(credentials.extras["plan"].as_str(), Some("pro"));
}

#[test]
fn an_api_key_codex_sign_in_cannot_be_bound() {
    let directory = Directory::new();
    let path = directory.auth_file(json!({"OPENAI_API_KEY":"sk-test","auth_mode":"apikey"}));
    assert_eq!(bind(&path).unwrap_err(), SourceError::NotSignedIn);
}

#[tokio::test]
async fn a_current_token_is_read_from_codex_without_renewing_it() {
    let directory = Directory::new();
    let token = access("account-1", 3600);
    let path = directory.auth_file(codex_file(&token, "account-1"));
    let credentials = bind(&path).unwrap();
    let client = client(json!({}));

    let resolved = resolve(&credentials, false, &client).await.unwrap();

    assert_eq!(resolved.access_token, token);
    assert_eq!(client.requests.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn an_expiring_token_is_renewed_into_codex_own_file() {
    let directory = Directory::new();
    let path = directory.auth_file(codex_file(&access("account-1", 60), "account-1"));
    let credentials = bind(&path).unwrap();
    let renewed = access("account-1", 3600);
    let client = client(json!({
        "access_token": renewed, "refresh_token": "refresh-2", "id_token": "id-2", "expires_in": 3600
    }));

    let resolved = resolve(&credentials, false, &client).await.unwrap();

    assert_eq!(resolved.access_token, renewed);
    assert_eq!(client.requests.load(Ordering::Relaxed), 1);
    let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert_eq!(saved["tokens"]["refresh_token"], "refresh-2");
    assert_eq!(saved["tokens"]["id_token"], "id-2");
    assert_eq!(saved["tokens"]["account_id"], "account-1");
    assert_eq!(saved["auth_mode"], "chatgpt");
    assert_ne!(saved["last_refresh"], "2026-01-01T00:00:00Z");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn a_different_codex_account_is_never_used_for_the_bound_one() {
    let directory = Directory::new();
    let path = directory.auth_file(codex_file(&access("account-1", 3600), "account-1"));
    let credentials = bind(&path).unwrap();
    directory.auth_file(codex_file(&access("account-2", 3600), "account-2"));

    let error = resolve(&credentials, false, &client(json!({})))
        .await
        .unwrap_err();

    assert_eq!(error, SourceError::AccountChanged);
}

fn user_access(subject: &str, workspace: &str, expires_in_seconds: i64) -> String {
    jwt(
        json!({"sub":subject,"exp":now_millis()/1000+expires_in_seconds,"https://api.openai.com/auth":{"chatgpt_account_id":workspace},"https://api.openai.com/profile":{"email":"shared@example.test"}}),
    )
}

#[tokio::test]
async fn switching_people_inside_one_workspace_rejects_the_previous_source_binding() {
    let directory = Directory::new();
    let path = directory.auth_file(codex_file(
        &user_access("person-a", "workspace", 3600),
        "workspace",
    ));
    let credentials = bind(&path).unwrap();
    assert_eq!(
        credentials.extras["accountIdentity"].as_value()["subject"],
        "person-a"
    );
    directory.auth_file(codex_file(
        &user_access("person-b", "workspace", 3600),
        "workspace",
    ));
    let client = client(json!({}));
    let error = resolve(&credentials, false, &client).await.unwrap_err();
    assert_eq!(error, SourceError::AccountChanged);
    assert_eq!(client.requests.load(Ordering::Relaxed), 0);
}

#[tokio::test]
async fn a_renewed_token_keeps_the_same_verified_person_and_workspace() {
    let directory = Directory::new();
    let path = directory.auth_file(codex_file(
        &user_access("person-a", "workspace", 60),
        "workspace",
    ));
    let credentials = bind(&path).unwrap();
    let token = user_access("person-a", "workspace", 3600);
    let client =
        client(json!({"access_token":token,"refresh_token":"next-refresh","expires_in":3600}));
    assert_eq!(
        resolve(&credentials, false, &client)
            .await
            .unwrap()
            .access_token,
        token
    );
}

#[tokio::test]
async fn a_renewed_token_for_another_person_is_not_used_or_written_into_codex() {
    let directory = Directory::new();
    let path = directory.auth_file(codex_file(
        &user_access("person-a", "workspace", 60),
        "workspace",
    ));
    let credentials = bind(&path).unwrap();
    let original = std::fs::read(&path).unwrap();
    let client = client(
        json!({"access_token":user_access("person-b","workspace",3600),"refresh_token":"other-person-refresh","expires_in":3600}),
    );
    assert_eq!(
        resolve(&credentials, false, &client).await.unwrap_err(),
        SourceError::AccountChanged
    );
    assert_eq!(std::fs::read(&path).unwrap(), original);
}

#[tokio::test]
async fn an_external_account_switch_while_renewal_waits_is_not_overwritten() {
    struct SwitchingClient {
        path: PathBuf,
        switched: Value,
    }
    #[async_trait]
    impl HttpClient for SwitchingClient {
        async fn stream(
            &self,
            _request: HttpRequest,
        ) -> Result<HttpStreamingResponse, HttpClientError> {
            unreachable!()
        }
        async fn request(&self, _request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
            std::fs::write(&self.path, self.switched.to_string()).unwrap();
            Ok(HttpResponse { status:200, headers:BTreeMap::new(), body:json!({"access_token":user_access("person-a","workspace",3600),"refresh_token":"person-a-next","expires_in":3600}).to_string().into_bytes() })
        }
    }
    let directory = Directory::new();
    let path = directory.auth_file(codex_file(
        &user_access("person-a", "workspace", 60),
        "workspace",
    ));
    let credentials = bind(&path).unwrap();
    let switched = codex_file(&user_access("person-b", "workspace", 3600), "workspace");
    let client = SwitchingClient {
        path: path.clone(),
        switched: switched.clone(),
    };
    assert_eq!(
        resolve(&credentials, false, &client).await.unwrap_err(),
        SourceError::AccountChanged
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&path).unwrap()).unwrap(),
        switched
    );
}
