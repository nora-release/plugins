use super::*;
use nora_ai::{HttpClientError, HttpRequest, HttpStreamingResponse};
use nora_provider::account_import::{self, CredentialPersistence, LoginKind};
use std::sync::Mutex;
#[path = "cpa_fixture.rs"]
mod fixture;
use fixture::*;

#[derive(Default)]
struct Mock {
    saved: Mutex<Vec<Value>>,
    requests: Mutex<Vec<HttpRequest>>,
    reply: Option<Value>,
    fail_save: bool,
}
#[async_trait]
impl CredentialPersistence for Mock {
    async fn save(&self, credentials: Value) -> PluginResult<()> {
        if self.fail_save {
            return Err(PluginError::invalid("private-storage-failure"));
        }
        self.saved.lock().unwrap().push(credentials);
        Ok(())
    }
}
#[async_trait]
impl HttpClient for Mock {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let body = self
            .reply
            .as_ref()
            .expect("CPA import must not contact a vendor")
            .to_string();
        Ok(HttpStreamingResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: Box::pin(futures::stream::iter([Ok(body.into_bytes().into())])),
        })
    }
}
fn kind() -> LoginKind {
    Xai::default()
        .account_import_kind()
        .expect("xAI must declare CPA account import")
}

#[test]
fn cpa_import_is_declared_with_the_rotation_notice_and_separate_from_cli_discovery() {
    let manifest: Value = serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
    assert_eq!(
        manifest["config"]["accountImport"],
        account_import::declaration(kind())
    );
    assert_eq!(
        manifest["config"]["accountImport"]["formats"],
        json!(["auto", "cli-proxy"])
    );
    assert_eq!(
        manifest["config"]["accountImport"]["refreshesTokens"],
        false
    );
    assert!(manifest["config"]["accountImport"]["notice"]
        .as_str()
        .unwrap()
        .contains("Stop using"));
    assert!(
        account_import::inspect(kind(), &params(&source())).unwrap()["entries"][0]["error"]
            .is_null()
    );
}

#[tokio::test]
async fn enabled_cpa_account_becomes_a_private_owned_copy_without_network_or_cli() {
    let input = source();
    let before = input.clone();
    let mock = Mock::default();
    let preview = account_import::inspect(kind(), &params(&input)).unwrap();
    assert_eq!(preview["entries"][0]["label"], "fixture@example.test");
    assert!(!preview.to_string().contains("private-cpa-refresh"));
    let imported = account_import::import_login(kind(), &params(&input), &mock, &mock)
        .await
        .unwrap();
    let saved = &imported["credentials"];
    assert_eq!(saved["type"], "oauth");
    assert_eq!(saved["credentialProvider"], "xAI");
    assert_eq!(saved["access"], input["access_token"]);
    assert_eq!(saved["refresh"], input["refresh_token"]);
    assert_eq!(saved["expires"], 4070908800000_i64);
    assert_eq!(saved["email"], input["email"]);
    assert_eq!(
        saved["accountIdentity"],
        json!({"issuer":"https://auth.x.ai","subject":"fixture-person","scope":"[\"\",\"\"]"})
    );
    assert!(saved.get("source").is_none());
    assert!(saved.pointer("/extras/noraAccountSource").is_none());
    assert_eq!(
        saved.pointer("/extras/noraImportState"),
        Some(&json!("complete"))
    );
    assert_eq!(*mock.saved.lock().unwrap(), vec![saved.clone()]);
    assert_eq!(imported["refreshed"], false);
    assert_eq!(input, before);
    assert!(mock.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn rejects_every_invalid_official_cpa_shape_before_saving_any_credentials() {
    let token = source()["access_token"].as_str().unwrap().to_string();
    let cases = vec![
        ("type", json!("openai")),
        ("auth_kind", json!("api_key")),
        ("disabled", json!(true)),
        ("disabled", Value::Null),
        ("access_token", json!("")),
        ("refresh_token", json!("")),
        ("sub", json!("")),
        ("email", json!("")),
        ("expired", json!("yesterday")),
        ("expired", json!(["2026-01-01T00:00:00Z"])),
        ("last_refresh", json!(1759936586)),
        (
            "access_token",
            json!(jwt(json!({"iss":"https://wrong.invalid","aud":[CLIENT]}))),
        ),
        (
            "access_token",
            json!(jwt(
                json!({"iss":"https://auth.x.ai","aud":["other-client"]})
            )),
        ),
        ("access_token", json!("opaque")),
        ("refresh_token", json!("header\r\ninjection")),
    ];
    for (field, value) in cases {
        let mut input = source();
        input[field] = value;
        let mock = Mock::default();
        let preview = account_import::inspect(kind(), &params(&input)).unwrap();
        assert!(
            preview["entries"][0]["error"].is_string(),
            "{field}: {preview}"
        );
        let error = account_import::import_login(kind(), &params(&input), &mock, &mock)
            .await
            .unwrap_err();
        assert_eq!(error.code, "invalid_account_import");
        assert!(!error.message.contains(&token));
        assert!(!error.message.contains("private-cpa-refresh"));
        assert!(mock.saved.lock().unwrap().is_empty());
        assert!(mock.requests.lock().unwrap().is_empty());
    }
    for content in ["not json", "null", "[]", "\"refresh\"", "{}"] {
        let mut p = params(&source());
        p["content"] = json!(content);
        assert!(
            account_import::import_login(kind(), &p, &Mock::default(), &Mock::default())
                .await
                .is_err()
        );
    }
}

#[tokio::test]
async fn expired_access_is_imported_and_then_renewed_with_the_cpa_client_without_changing_identity()
{
    let mut input = source();
    input["expired"] = json!("2000-01-01T00:00:00Z");
    let mock = Mock {
        reply: Some(
            json!({"access_token":"private-new-access","refresh_token":"private-new-refresh","expires_in":3600}),
        ),
        ..Default::default()
    };
    let imported = account_import::import_login(kind(), &params(&input), &mock, &mock)
        .await
        .unwrap();
    assert_eq!(imported["credentials"]["expires"], 946684800000_i64);
    assert!(mock.requests.lock().unwrap().is_empty());
    let saved: OAuthCredentials = serde_json::from_value(imported["credentials"].clone()).unwrap();
    let refreshed = Xai::default().refresh(&saved, &mock).await.unwrap();
    assert_eq!(refreshed.access_token, "private-new-access");
    assert_eq!(
        refreshed.refresh_token.as_deref(),
        Some("private-new-refresh")
    );
    assert_eq!(refreshed.extras, saved.extras);
    let requests = mock.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, "https://auth.x.ai/oauth2/token");
    let body = String::from_utf8(requests[0].body.clone().unwrap().to_vec()).unwrap();
    assert!(body.contains(&format!("client_id={CLIENT}")));
    assert!(body.contains("refresh_token=private-cpa-refresh"));
}

#[tokio::test]
async fn import_save_failure_never_reports_success_and_retry_resume_stays_with_the_same_account() {
    let input = source();
    let failing = Mock {
        fail_save: true,
        ..Default::default()
    };
    let error = account_import::import_login(kind(), &params(&input), &failing, &failing)
        .await
        .unwrap_err();
    assert_eq!(error.code, "credential_persistence");
    assert!(!error.message.contains("private-storage"));
    let mock = Mock::default();
    let original = account_import::import_login(kind(), &params(&input), &mock, &mock)
        .await
        .unwrap()["credentials"]
        .clone();
    let mut resume = params(&input);
    resume["credentials"] = original.clone();
    resume["credentials"]["refresh"] = json!("private-rotated-refresh");
    let recovered = account_import::import_login(kind(), &resume, &mock, &mock)
        .await
        .unwrap();
    assert_eq!(
        recovered["credentials"]["refresh"],
        "private-rotated-refresh"
    );
    assert_eq!(recovered["accountIdentity"], original["accountIdentity"]);
    let mut other = source();
    other["refresh_token"] = json!("other-refresh");
    resume["content"] = json!(other.to_string());
    assert!(account_import::import_login(kind(), &resume, &mock, &mock)
        .await
        .is_err());
}

#[test]
fn scalar_audience_duplicate_detection_and_changed_file_binding_match_import_contract() {
    let mut scalar = source();
    scalar["access_token"] = json!(jwt(json!({"iss":"https://auth.x.ai","aud":CLIENT})));
    let mut p = params(&json!([source(), source(), scalar]));
    p["format"] = json!("cli-proxy");
    let preview = account_import::inspect(kind(), &p).unwrap();
    assert_eq!(preview["entries"][1]["duplicateOf"], 0);
    assert_eq!(preview["entries"][2]["duplicateOf"], 0);
    assert!(!preview.to_string().contains("private-cpa-refresh"));
}

#[tokio::test]
async fn cpa_timestamp_strings_keep_the_same_instants_and_accept_expired_dates() {
    for (timestamp, expected) in [
        ("2026-10-20T00:00:00Z", 1792454400000_i64),
        ("2026-10-20T08:00:00+08:00", 1792454400000),
        ("2026-10-20T00:00Z", 1792454400000),
        ("2026-10-20", 1792454400000),
        ("2026-10", 1790812800000),
        ("2026", 1767225600000),
        ("Tue, 20 Oct 2026 00:00:00 GMT", 1792454400000),
        ("2000-01-01T00:00:00Z", 946684800000),
    ] {
        let mut input = source();
        input["expired"] = json!(timestamp);
        input["last_refresh"] = json!(timestamp);
        let mock = Mock::default();
        let imported = account_import::import_login(kind(), &params(&input), &mock, &mock)
            .await
            .unwrap();
        assert_eq!(imported["credentials"]["expires"], expected, "{timestamp}");
        assert_eq!(
            imported["credentials"]["extras"]["createdAt"], expected,
            "{timestamp}"
        );
    }
}

#[tokio::test]
async fn cpa_resume_cannot_substitute_a_source_other_account_or_oauth_client() {
    let input = source();
    let mock = Mock::default();
    let original = account_import::import_login(kind(), &params(&input), &mock, &mock)
        .await
        .unwrap()["credentials"]
        .clone();
    for pointer in [
        "/credentialProvider",
        "/accountIdentity/subject",
        "/extras/accountIdentity/subject",
        "/extras/oidcIssuer",
        "/extras/oidcClientId",
        "/extras/noraImportFingerprint",
    ] {
        let mut p = params(&input);
        p["credentials"] = original.clone();
        *p["credentials"].pointer_mut(pointer).unwrap() = json!("other-account");
        let guard = Mock::default();
        assert!(
            account_import::import_login(kind(), &p, &guard, &guard)
                .await
                .is_err(),
            "{pointer}"
        );
        assert!(guard.saved.lock().unwrap().is_empty());
    }
    let mut p = params(&input);
    p["fingerprint"] = json!("changed");
    assert!(account_import::import_login(kind(), &p, &mock, &mock)
        .await
        .is_err());
    p = params(&input);
    p["credentials"] = original;
    p["credentials"]["source"] = json!({"kind":"grokCLI"});
    assert!(account_import::import_login(kind(), &p, &mock, &mock)
        .await
        .is_err());
}
