use super::*;
use async_trait::async_trait;
use base64::Engine;
use nora_ai::{HttpClientError, HttpRequest, HttpStreamingResponse};
use std::sync::Mutex;
#[path = "fixture.rs"]
mod fixture;

struct Harness {
    client: fixture::Client,
    events: Mutex<Vec<String>>,
    saves: Mutex<Vec<Value>>,
    fail_save: Option<usize>,
}
impl Harness {
    fn new(replies: Vec<fixture::Reply>) -> Self {
        Self {
            client: fixture::Client::new(replies),
            events: Mutex::new(Vec::new()),
            saves: Mutex::new(Vec::new()),
            fail_save: None,
        }
    }
}
#[async_trait]
impl HttpClient for Harness {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.events.lock().unwrap().push(format!(
            "request:{}",
            if request.url == TOKEN {
                "token"
            } else if request.url == USER_INFO {
                "identity"
            } else if request.url == VERSION {
                "version"
            } else {
                "project"
            }
        ));
        self.client.stream(request).await
    }
}
#[async_trait]
impl CredentialPersistence for Harness {
    async fn save(&self, credentials: Value) -> PluginResult<()> {
        let mut saves = self.saves.lock().unwrap();
        self.events.lock().unwrap().push(format!(
            "save:{}",
            credentials
                .pointer("/extras/noraImportState")
                .and_then(Value::as_str)
                .unwrap_or_default()
        ));
        if self.fail_save == Some(saves.len() + 1) {
            return Err(PluginError::invalid("private-persistence-error"));
        }
        saves.push(credentials);
        Ok(())
    }
}
fn params(value: Value) -> Value {
    json!({"format":"auto","content":value.to_string(),"credentials":{"type":"pendingImport","configured":true,"enabled":true}})
}
fn token_reply() -> fixture::Reply {
    fixture::Reply::json(
        json!({"access_token":"new-private-access","refresh_token":"new-private-refresh","expires_in":3600}),
    )
}
fn identity_reply() -> fixture::Reply {
    fixture::Reply::json(
        json!({"id":"google-user-1","email":"actual@example.test","name":"Actual","verified_email":true}),
    )
}
fn project_reply() -> fixture::Reply {
    fixture::Reply::json(
        json!({"cloudaicompanionProject":"actual-project","currentTier":{"id":"paid","name":"Google AI Pro"}}),
    )
}
fn account() -> Value {
    json!({"email":"file@example.test","refresh_token":"original-private-refresh","project_id":"file-project"})
}

#[test]
fn google_parser_matches_each_magpie_reference_shape() {
    let cases = [
        (
            "google-account",
            json!([{"email":"a@x.test","refresh_token":"1//a"},{"email":"b@x.test","refresh_token":"1//b"}]),
            2,
        ),
        (
            "google-account",
            json!({"email":"a@x.test","refreshToken":"1//a","projectId":"p1"}),
            1,
        ),
        (
            "antigravity-manager",
            json!({"accounts":[{"email":"a@x.test","refresh_token":"1//a"}]}),
            1,
        ),
        (
            "antigravity-manager",
            json!({"id":"user1","email":"a@x.test","token":{"access_token":"ya29.private","refresh_token":"1//a","project_id":"proj-a"}}),
            1,
        ),
        (
            "cli-proxy",
            json!({"type":"antigravity","access_token":"ya29.private","refresh_token":"1//a","email":"a@x.test","project_id":"proj-a"}),
            1,
        ),
        (
            "nora-account",
            json!({"type":"oauth","app":"antigravity","access":"a","refresh":"r","email":"a@x.test"}),
            1,
        ),
        (
            "magpie-login",
            json!({"agent":"antigravity","user":"a@x.test","auth":json!({"refresh_token":"r","project":"p"}).to_string()}),
            1,
        ),
    ];
    for (format, value, count) in cases {
        let mut params = params(value);
        params["format"] = json!(format);
        let result = inspect(App::Antigravity, &params).unwrap();
        assert_eq!(
            result["entries"].as_array().unwrap().len(),
            count,
            "{format}"
        );
        for item in result["entries"].as_array().unwrap() {
            assert!(item["error"].is_null(), "{format}: {result}");
        }
    }
}
#[test]
fn google_raw_lines_bom_and_json_token_arrays_are_supported() {
    for content in [
        "\u{feff}1//a\n\n# comment\n1//b\r\n".to_string(),
        json!(["1//a", "1//b"]).to_string(),
    ] {
        let result = inspect(
            App::Antigravity,
            &json!({"content":content,"format":"refresh-token"}),
        )
        .unwrap();
        assert_eq!(result["entries"].as_array().unwrap().len(), 2);
        assert!(!result.to_string().contains("1//"));
    }
}
#[test]
fn duplicate_tokens_and_case_insensitive_emails_are_identified_without_leaking_tokens() {
    let result = inspect(
        App::Antigravity,
        &params(json!([
            {"email":"a@x.test","refresh_token":"1//private-a"},
            {"email":"A@x.test","refresh_token":"1//private-z"},
            {"email":"b@x.test","refresh_token":"1//private-a"},
            {"email":"c@x.test"},3
        ])),
    )
    .unwrap();
    assert_eq!(result["entries"][1]["duplicateOf"], 0);
    assert_eq!(result["entries"][2]["duplicateOf"], 0);
    assert!(result["entries"][3]["error"].is_string());
    assert!(result["entries"][4]["error"].is_string());
    assert!(!result.to_string().contains("1//private"));
}
#[test]
fn files_for_other_apps_clients_or_local_paths_are_refused() {
    let claims = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(json!({"aud":App::GeminiCLI.client_id()}).to_string());
    let cases = [
        json!({"type":"gemini","refresh_token":"private"}),
        json!({"type":"oauth","app":"gemini-cli","refresh":"private"}),
        json!({"type":"codex","refresh_token":"private"}),
        json!({"refresh_token":"private","client_id":App::GeminiCLI.client_id()}),
        json!({"refresh_token":"private","token":{"clientId":"another-client"}}),
        json!({"refresh_token":"private","id_token":format!("header.{claims}.signature")}),
        json!({"refresh_token":"private","source":{"path":"/private/auth.json"}}),
        json!({"refresh_token":"private","extras":{"noraAccountSource":{"path":"/private/auth.json"}}}),
        json!({"access_token":"private"}),
        json!({"refresh_token":"private token"}),
        json!({"refresh_token":"private","project":"invalid project"}),
    ];
    for value in cases {
        let result = inspect(App::Antigravity, &params(value)).unwrap();
        assert!(result["entries"][0]["error"].is_string(), "{result}");
        assert!(!result.to_string().contains("private"));
    }
    for kind in ["gemini", "gemini-cli", "authorized_user"] {
        let result=inspect(App::GeminiCLI,&params(json!({"type":kind,"client_id":App::GeminiCLI.client_id(),"refresh_token":"private"}))).unwrap();
        assert!(result["entries"][0]["error"].is_null());
    }
}
#[test]
fn invalid_json_formats_bounds_and_filenames_have_fixed_safe_errors() {
    for params in [
        json!({"content":"{private broken"}),
        json!({"content":"private","format":"google-account"}),
        json!({"content":"private","format":"unknown"}),
        json!({"content":"private","fileName":"bad\nprivate"}),
        json!({"content":json!(vec!["private";201]).to_string()}),
        json!({"content":"x".repeat(shared::MAX_BYTES+1)}),
        json!({"content":"[]"}),
    ] {
        assert!(!inspect(App::Antigravity, &params)
            .unwrap_err()
            .message
            .contains("private"));
    }
}
#[tokio::test]
async fn google_rotation_is_saved_before_identity_project_and_correct_app_client_is_used() {
    for app in [App::GeminiCLI, App::Antigravity] {
        let plugin = CodeAssist::new(app);
        let fixture = Harness::new(vec![token_reply(), identity_reply(), project_reply()]);
        let result = import(&plugin, &params(account()), &fixture, &fixture)
            .await
            .unwrap();
        let mut expected = vec![
            "save:pending",
            "request:token",
            "save:rotated",
            "request:identity",
            "save:identified",
        ];
        if app == App::Antigravity {
            expected.push("request:version");
        }
        expected.extend(["request:project", "save:complete"]);
        assert_eq!(*fixture.events.lock().unwrap(), expected);
        assert_eq!(
            result["accountIdentity"],
            json!({"issuer":"https://accounts.google.com","subject":"google-user-1"})
        );
        assert_eq!(result["profile"]["email"], "actual@example.test");
        assert_eq!(result["profile"]["plan"], "Google AI Pro");
        assert_eq!(
            fixture.saves.lock().unwrap()[1]["refresh"],
            "new-private-refresh"
        );
        let requests = fixture.client.requests.lock().unwrap();
        let form = url::form_urlencoded::parse(requests[0].body.as_ref().unwrap())
            .collect::<BTreeMap<_, _>>();
        assert_eq!(form["client_id"], app.client_id());
        assert_eq!(form["refresh_token"], "original-private-refresh");
    }
}
#[tokio::test]
async fn persistence_refusal_stops_further_vendor_requests() {
    for failure in [1, 2, 3] {
        let plugin = CodeAssist::new(App::GeminiCLI);
        let fixture = Harness {
            fail_save: Some(failure),
            ..Harness::new(vec![token_reply(), identity_reply(), project_reply()])
        };
        let error = import(&plugin, &params(account()), &fixture, &fixture)
            .await
            .unwrap_err();
        assert_eq!(fixture.client.requests.lock().unwrap().len(), failure - 1);
        assert_eq!(error.code, "credential_persistence");
        assert!(!error.message.contains("private"));
    }
}
#[tokio::test]
async fn incomplete_token_response_keeps_rotation_and_retry_uses_saved_value() {
    let plugin = CodeAssist::new(App::GeminiCLI);
    let first = Harness::new(vec![fixture::Reply::json(
        json!({"refresh_token":"rotated-private"}),
    )]);
    let error = import(&plugin, &params(account()), &first, &first)
        .await
        .unwrap_err();
    assert_eq!(error.code, "upstream_transport_error");
    let saved = first.saves.lock().unwrap().last().unwrap().clone();
    assert_eq!(saved["refresh"], "rotated-private");
    let mut params = params(account());
    params["credentials"] = saved;
    let second = Harness::new(vec![token_reply(), identity_reply(), project_reply()]);
    import(&plugin, &params, &second, &second).await.unwrap();
    let requests = second.client.requests.lock().unwrap();
    let body = String::from_utf8_lossy(requests[0].body.as_ref().unwrap());
    assert!(body.contains("rotated-private"));
    assert!(!body.contains("original-private-refresh"));
}
#[tokio::test]
async fn identity_failure_retains_new_tokens_and_file_email_stays_unverified() {
    let plugin = CodeAssist::new(App::GeminiCLI);
    let fixture = Harness::new(vec![
        token_reply(),
        fixture::Reply::raw(401, b"new-private-refresh"),
        project_reply(),
    ]);
    let result = import(&plugin, &params(account()), &fixture, &fixture)
        .await
        .unwrap();
    assert_eq!(
        result["accountIdentity"]["issuer"],
        "urn:nora:import:google-unverified"
    );
    assert_eq!(result["profile"]["email"], "file@example.test");
    let mut value = account();
    value.as_object_mut().unwrap().remove("email");
    let fixture = Harness::new(vec![
        token_reply(),
        fixture::Reply::raw(401, b"new-private-refresh"),
    ]);
    let error = import(&plugin, &params(value), &fixture, &fixture)
        .await
        .unwrap_err();
    assert!(!error.message.contains("private"));
    assert_eq!(
        fixture.saves.lock().unwrap().last().unwrap()["refresh"],
        "new-private-refresh"
    );
}
#[tokio::test]
async fn resume_after_identity_failure_does_not_rotate_again() {
    let plugin = CodeAssist::new(App::GeminiCLI);
    let mut value = account();
    value.as_object_mut().unwrap().remove("email");
    let first = Harness::new(vec![token_reply(), fixture::Reply::raw(401, b"private")]);
    assert!(import(&plugin, &params(value.clone()), &first, &first)
        .await
        .is_err());
    let mut params = params(value);
    params["credentials"] = first.saves.lock().unwrap().last().unwrap().clone();
    let second = Harness::new(vec![identity_reply(), project_reply()]);
    let result = import(&plugin, &params, &second, &second).await.unwrap();
    assert!(second
        .client
        .requests
        .lock()
        .unwrap()
        .iter()
        .all(|request| request.url != TOKEN));
    assert_eq!(result["credentials"]["refresh"], "new-private-refresh");
}
#[tokio::test]
async fn resume_must_match_file_index_and_application() {
    let plugin = CodeAssist::new(App::GeminiCLI);
    for patch in [
        json!({"index":-1}),
        json!({"index":1}),
        json!({"fingerprint":"wrong"}),
        json!({"credentials":{"type":"oauth","app":"antigravity","refresh":"private","extras":{"noraImportFingerprint":shared::fingerprint("gemini-cli","original-private-refresh")}}}),
    ] {
        let mut params = params(account());
        params
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        let fixture = Harness::new(vec![]);
        assert!(import(&plugin, &params, &fixture, &fixture).await.is_err());
        assert!(fixture.client.requests.lock().unwrap().is_empty());
        assert!(fixture.saves.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn vendor_errors_and_profile_metadata_do_not_expose_tokens() {
    let plugin = CodeAssist::new(App::GeminiCLI);
    let fixture = Harness::new(vec![fixture::Reply::raw(400, b"original-private-refresh")]);
    assert!(!import(&plugin, &params(account()), &fixture, &fixture)
        .await
        .unwrap_err()
        .message
        .contains("private"));
    let fixture = Harness::new(vec![
        token_reply(),
        fixture::Reply::json(
            json!({"id":"id","email":"new-private-refresh@example.test","name":"new-private-access"}),
        ),
        project_reply(),
    ]);
    let result = import(&plugin, &params(account()), &fixture, &fixture)
        .await
        .unwrap();
    assert!(!result["profile"].to_string().contains("new-private"));
}
#[tokio::test]
async fn same_google_identity_is_stable_across_different_refresh_tokens() {
    let mut identities = Vec::new();
    for refresh in ["private-one", "private-two"] {
        let plugin = CodeAssist::new(App::GeminiCLI);
        let fixture = Harness::new(vec![token_reply(), identity_reply(), project_reply()]);
        identities.push(
            import(
                &plugin,
                &params(json!({"refresh_token":refresh})),
                &fixture,
                &fixture,
            )
            .await
            .unwrap()["accountIdentity"]
                .clone(),
        );
    }
    assert_eq!(identities[0], identities[1]);
}
#[test]
fn account_import_manifests_advertise_only_the_implemented_contract() {
    for manifest in [
        include_str!("../../antigravity/nora-plugin.json"),
        include_str!("../../gemini-cli/nora-plugin.json"),
    ] {
        let manifest: Value = serde_json::from_str(manifest).unwrap();
        assert_eq!(
            manifest["contributes"]["provider"]["config"]["accountImport"]["formats"],
            json!(FORMATS)
        );
        assert_eq!(
            manifest["contributes"]["provider"]["config"]["accountImport"]["refreshesTokens"],
            true
        );
    }
}
