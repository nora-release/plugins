use super::*;
use base64::Engine;
use nora_ai::{HttpClient, HttpClientError, HttpRequest, HttpStreamingResponse};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};

#[derive(Clone, Default)]
struct Fixture {
    events: Arc<Mutex<Vec<String>>>,
    saves: Arc<Mutex<Vec<Value>>>,
    requests: Arc<Mutex<Vec<HttpRequest>>>,
    replies: Arc<Mutex<VecDeque<(u16, Value)>>>,
    fail_save: Option<usize>,
}
#[async_trait]
impl HttpClient for Fixture {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.events.lock().unwrap().push("request".into());
        self.requests.lock().unwrap().push(request);
        let (status, reply) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("unexpected vendor request");
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::new(),
            body: Box::pin(futures::stream::iter([Ok(reply
                .to_string()
                .into_bytes()
                .into())])),
        })
    }
}
#[async_trait]
impl CredentialPersistence for Fixture {
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
            return Err(PluginError::invalid("secret-in-storage-error"));
        }
        saves.push(credentials);
        Ok(())
    }
}
fn jwt(claims: Value) -> String {
    format!(
        "eyJhbGciOiJub25lIn0.{}.signature",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}
fn access() -> String {
    jwt(
        json!({"sub":"person-1","email":"actual@example.test","https://api.openai.com/auth":{"chatgpt_user_id":"person-1","chatgpt_account_id":"workspace-1","chatgpt_plan_type":"pro"}}),
    )
}
fn reply() -> Value {
    json!({"access_token":access(),"refresh_token":"new-private-refresh","expires_in":3600})
}
fn fixture(replies: Vec<Value>) -> Fixture {
    Fixture {
        replies: Arc::new(Mutex::new(
            replies.into_iter().map(|value| (200, value)).collect(),
        )),
        ..Default::default()
    }
}
fn params(value: Value) -> Value {
    json!({"fileName":"import.json","format":"auto","content":value.to_string(),"index":0,"credentials":{"type":"pendingImport","configured":true,"enabled":true}})
}
fn codex() -> Value {
    json!({"email":"file@example.test","tokens":{"access_token":"old-private-access","refresh_token":"old-private-refresh","id_token":jwt(json!({"sub":"forged-person","https://api.openai.com/auth":{"chatgpt_account_id":"forged-workspace"}}))}})
}
fn claude() -> Value {
    json!({"email":"claude@example.test","claudeAiOauth":{"accessToken":"private-claude-access","refreshToken":"private-claude-refresh","expiresAt":9999999999999_i64,"scopes":["user:inference","user:profile"]}})
}

#[test]
fn codex_formats_match_magpie_import_fixtures() {
    let cases = [
        ("codex-auth", codex()),
        (
            "cli-proxy",
            json!({"type":"codex","email":"a@example.test","refresh_token":"r"}),
        ),
        (
            "cockpit",
            json!({"refreshToken":"r","email":"a@example.test"}),
        ),
        (
            "sub2api",
            json!({"accounts":[{"platform":"openai","type":"oauth","name":"a@example.test","credentials":{"refresh_token":"r","chatgpt_account_id":"a"},"extra":{"plan":"pro"}}]}),
        ),
        (
            "codexbar",
            json!({"providers":[{"kind":"openai_oauth","accounts":[{"kind":"oauth_tokens","refreshToken":"r","email":"a@example.test","openAIAccountId":"a"}]}]}),
        ),
        (
            "nora-account",
            json!({"type":"oauth","credentialProvider":"openAICodex","access":"a","refresh":"r","email":"a@example.test"}),
        ),
        (
            "magpie-login",
            json!({"agent":"codex","user":"a@example.test","auth":codex().to_string()}),
        ),
    ];
    for (format, value) in cases {
        let mut params = params(value);
        params["format"] = json!(format);
        let result = inspect(LoginKind::Codex, &params).unwrap();
        assert!(
            result["entries"][0]["error"].is_null(),
            "{format}: {result}"
        );
        assert_eq!(
            result["entries"][0]["fingerprint"].as_str().unwrap().len(),
            64
        );
    }
}
#[test]
fn claude_formats_match_magpie_import_fixtures() {
    let cases = [
        ("claude-credentials", claude()),
        (
            "cli-proxy",
            json!({"type":"claude","email":"a@example.test","refresh_token":"r","access_token":"a"}),
        ),
        (
            "sub2api",
            json!({"accounts":[{"platform":"anthropic","type":"oauth","credentials":{"refresh_token":"r","access_token":"a","scope":"user:profile user:inference"},"extra":{"email_address":"a@example.test","account_uuid":"uuid-a","org_uuid":"org-b"}}]}),
        ),
        (
            "nora-account",
            json!({"type":"oauth","credentialProvider":"anthropic","access":"a","refresh":"r","email":"a@example.test"}),
        ),
        (
            "magpie-login",
            json!({"agent":"claude","user":"a@example.test","auth":claude(),"profile":json!({"emailAddress":"a@example.test","accountUuid":"uuid-a","organizationUuid":"org-b"}).to_string()}),
        ),
    ];
    for (format, value) in cases {
        let mut params = params(value);
        params["format"] = json!(format);
        let result = inspect(LoginKind::Claude, &params).unwrap();
        assert!(
            result["entries"][0]["error"].is_null(),
            "{format}: {result}"
        );
    }
}
#[test]
fn refresh_lines_arrays_and_bom_are_supported_without_secret_labels() {
    for content in [
        "\u{feff}# exported\nprivate-r1\n\nprivate-r2".into(),
        json!(["private-r1", "private-r2"]).to_string(),
    ] {
        let result = inspect(
            LoginKind::Codex,
            &json!({"format":"refresh-token","content":content}),
        )
        .unwrap();
        assert_eq!(result["entries"].as_array().unwrap().len(), 2);
        assert_eq!(result["entries"][0]["label"], "#1");
        assert!(!result.to_string().contains("private-r"));
    }
}
#[test]
fn duplicate_refresh_tokens_are_identified_before_import() {
    let result = inspect(LoginKind::Codex, &params(json!([codex(), codex()]))).unwrap();
    assert!(result["entries"][0]["duplicateOf"].is_null());
    assert_eq!(result["entries"][1]["duplicateOf"], 0);
    assert_eq!(
        result["entries"][0]["fingerprint"],
        result["entries"][1]["fingerprint"]
    );
    assert_ne!(
        fingerprint("openai", "same"),
        fingerprint("anthropic", "same")
    );
}
#[test]
fn codexbar_repeated_sections_do_not_create_duplicate_entries() {
    let account = json!({"kind":"oauth_tokens","refreshToken":"r","email":"a@example.test"});
    let value = json!({"providers":[{"kind":"other","accounts":[{"refreshToken":"other"}]},{"kind":"openai_oauth","accounts":[account]}],"openAI":{"remoteConnectionAccounts":[account]}});
    assert_eq!(
        inspect(LoginKind::Codex, &params(value)).unwrap()["entries"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
    assert!(inspect(
        LoginKind::Codex,
        &params(json!({"openAI":{"remoteConnectionAccounts":[account]}}))
    )
    .unwrap()["entries"][0]["error"]
        .is_null());
}
#[test]
fn cross_provider_api_key_and_local_source_exports_are_refused() {
    for (kind, value) in [
        (LoginKind::Codex, claude()),
        (LoginKind::Claude, codex()),
        (
            LoginKind::Codex,
            json!({"type":"claude","refresh_token":"private"}),
        ),
        (LoginKind::Codex, json!({"OPENAI_API_KEY":"private"})),
        (
            LoginKind::Codex,
            json!({"tokens":{"access_token":"private"}}),
        ),
        (
            LoginKind::Codex,
            json!({"refresh":"private","source":{"path":"/private/auth.json"}}),
        ),
        (
            LoginKind::Codex,
            json!({"refresh":"private","extras":{"noraAccountSource":{"path":"/private/auth.json"}}}),
        ),
        (
            LoginKind::Claude,
            json!({"email":"a@example.test","refresh_token":"private"}),
        ),
        (
            LoginKind::Claude,
            json!({"access_token":"private","refresh_token":"private"}),
        ),
        (
            LoginKind::Codex,
            json!({"platform":"anthropic","type":"oauth","credentials":{"refresh_token":"private"}}),
        ),
    ] {
        let result = inspect(kind, &params(value)).unwrap();
        assert!(result["entries"][0]["error"].is_string(), "{result}");
        assert!(!result.to_string().contains("private"));
    }
}
#[test]
fn explicit_formats_limits_and_bad_metadata_fail_with_safe_messages() {
    for params in [
        json!({"content":"private secret","format":"unknown"}),
        json!({"content":"private-secret","format":"codex-auth"}),
        json!({"content":codex().to_string(),"format":"codexbar"}),
        json!({"content":"{ private-malformed"}),
        json!({"content":"x".repeat(MAX_BYTES+1)}),
        json!({"content":json!(vec!["private";201]).to_string()}),
        json!({"content":"private","fileName":"bad\nprivate"}),
    ] {
        let error = inspect(LoginKind::Codex, &params).unwrap_err();
        assert!(!error.message.contains("private"));
    }
    for value in [
        json!({"refresh":"private secret"}),
        json!({"refresh":"private","plan":"x\ny"}),
    ] {
        let result = inspect(LoginKind::Codex, &params(value)).unwrap();
        assert!(result["entries"][0]["error"].is_string());
    }
}
#[test]
fn inspect_redacts_tokens_smuggled_into_display_fields() {
    let result = inspect(
        LoginKind::Codex,
        &params(json!({"email":"private-token@example.test","refresh_token":"private-token"})),
    )
    .unwrap();
    assert!(!result.to_string().contains("private-token"));
}
#[tokio::test]
async fn codex_checkpoints_before_and_immediately_after_rotation_and_uses_fresh_identity() {
    let fixture = fixture(vec![reply()]);
    let result = import_login(LoginKind::Codex, &params(codex()), &fixture, &fixture)
        .await
        .unwrap();
    assert_eq!(
        *fixture.events.lock().unwrap(),
        ["save:pending", "request", "save:rotated", "save:complete"]
    );
    let saves = fixture.saves.lock().unwrap();
    assert_eq!(saves[0]["refresh"], "old-private-refresh");
    assert_eq!(saves[1]["refresh"], "new-private-refresh");
    assert_eq!(
        result["accountIdentity"],
        json!({"issuer":"https://auth.openai.com","subject":"person-1","scope":"workspace-1"})
    );
    assert_eq!(result["profile"]["email"], "actual@example.test");
    assert_eq!(result["credentials"]["credentialProvider"], "openAICodex");
    let requests = fixture.requests.lock().unwrap();
    assert_eq!(requests[0].url, "https://auth.openai.com/oauth/token");
    let form =
        url::form_urlencoded::parse(requests[0].body.as_ref().unwrap()).collect::<BTreeMap<_, _>>();
    assert_eq!(form["refresh_token"], "old-private-refresh");
    assert_eq!(form["grant_type"], "refresh_token");
}
#[tokio::test]
async fn unavailable_storage_stops_before_refresh_and_errors_do_not_leak() {
    for failed_save in [1, 2] {
        let fixture = Fixture {
            fail_save: Some(failed_save),
            ..fixture(vec![reply()])
        };
        let error = import_login(LoginKind::Codex, &params(codex()), &fixture, &fixture)
            .await
            .unwrap_err();
        assert_eq!(
            fixture.requests.lock().unwrap().len(),
            usize::from(failed_save == 2)
        );
        assert!(!error.message.contains("secret"));
        assert_eq!(error.code, "credential_persistence");
        assert!(!fixture
            .events
            .lock()
            .unwrap()
            .iter()
            .any(|event| event == "save:complete"));
    }
}
#[tokio::test]
async fn rotated_refresh_is_saved_even_when_remaining_response_is_incomplete() {
    for payload in [
        json!({"refresh_token":"rotated-private"}),
        json!({"refresh_token":"rotated-private","access_token":"new-access","expires_in":-1}),
    ] {
        let fixture = fixture(vec![payload]);
        let error = import_login(LoginKind::Codex, &params(codex()), &fixture, &fixture)
            .await
            .unwrap_err();
        assert_eq!(
            fixture.saves.lock().unwrap().last().unwrap()["refresh"],
            "rotated-private"
        );
        assert!(!error.message.contains("private"));
    }
}
#[tokio::test]
async fn resumed_failed_import_uses_the_saved_refresh_token_not_the_original_file() {
    let first = fixture(vec![json!({"refresh_token":"rotated-private"})]);
    assert!(
        import_login(LoginKind::Codex, &params(codex()), &first, &first)
            .await
            .is_err()
    );
    let mut params = params(codex());
    params["credentials"] = first.saves.lock().unwrap().last().unwrap().clone();
    let second = fixture(vec![reply()]);
    import_login(LoginKind::Codex, &params, &second, &second)
        .await
        .unwrap();
    let request = &second.requests.lock().unwrap()[0];
    let body = String::from_utf8_lossy(request.body.as_ref().unwrap());
    assert!(body.contains("rotated-private"));
    assert!(!body.contains("old-private-refresh"));
}
#[tokio::test]
async fn resumed_live_rotated_credentials_do_not_reuse_a_single_use_refresh_token() {
    let first = fixture(vec![reply()]);
    let mut params = params(codex());
    import_login(LoginKind::Codex, &params, &first, &first)
        .await
        .unwrap();
    params["credentials"] = first.saves.lock().unwrap()[1].clone();
    let second = fixture(vec![]);
    let result = import_login(LoginKind::Codex, &params, &second, &second)
        .await
        .unwrap();
    assert_eq!(result["credentials"]["refresh"], "new-private-refresh");
    assert!(second.requests.lock().unwrap().is_empty());
}
#[tokio::test]
async fn changed_selection_or_forged_resume_is_rejected_without_network() {
    let base = params(codex());
    for patch in [
        json!({"index":-1}),
        json!({"index":8}),
        json!({"index":"0"}),
        json!({"fingerprint":"different"}),
        json!({"credentials":{"type":"oauth","refresh":"private","extras":{"noraImportFingerprint":"different"}}}),
        json!({"credentials":{"type":"apiKey","refresh":"private","extras":{"noraImportFingerprint":fingerprint("openai","old-private-refresh")}}}),
    ] {
        let mut params = base.clone();
        params
            .as_object_mut()
            .unwrap()
            .extend(patch.as_object().unwrap().clone());
        let fixture = fixture(vec![]);
        assert!(import_login(LoginKind::Codex, &params, &fixture, &fixture)
            .await
            .is_err());
        assert!(fixture.requests.lock().unwrap().is_empty());
        assert!(fixture.saves.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn refreshed_tokens_without_identity_cannot_merge_using_forged_file_claims() {
    let fixture = fixture(vec![
        json!({"access_token":"opaque-access","refresh_token":"new-refresh","expires_in":3600}),
    ]);
    let result = import_login(LoginKind::Codex, &params(codex()), &fixture, &fixture)
        .await
        .unwrap();
    assert_eq!(
        result["accountIdentity"]["issuer"],
        "urn:nora:import:openai-unverified"
    );
    assert!(!result["accountIdentity"].to_string().contains("forged"));
}
#[tokio::test]
async fn different_exports_of_the_same_verified_account_have_one_identity() {
    let mut identities = Vec::new();
    for refresh in ["private-1", "private-2"] {
        let fixture = fixture(vec![reply()]);
        identities.push(
            import_login(
                LoginKind::Codex,
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
#[tokio::test]
async fn magpie_codex_response_can_derive_expiry_from_access_jwt_and_keep_workspaces_distinct() {
    let mut identities = Vec::new();
    for workspace in ["workspace-a", "workspace-b"] {
        let access = jwt(json!({"exp":nora_ai::oauth::now_millis()/1000+3600}));
        let id = jwt(
            json!({"email":"team@example.test","https://api.openai.com/auth":{"chatgpt_account_id":workspace,"chatgpt_plan_type":"team"}}),
        );
        let fixture = fixture(vec![
            json!({"access_token":access,"id_token":id,"refresh_token":"new-private-refresh"}),
        ]);
        let result = import_login(LoginKind::Codex, &params(codex()), &fixture, &fixture)
            .await
            .unwrap();
        assert_eq!(
            result["accountIdentity"]["issuer"],
            "urn:nora:import:openai-email"
        );
        assert_eq!(result["accountIdentity"]["scope"], workspace);
        identities.push(result["accountIdentity"].clone());
    }
    assert_ne!(identities[0], identities[1]);
}
#[tokio::test]
async fn claude_import_defers_renewal_to_cli_and_keeps_known_uuid_identity() {
    let mut value = claude();
    value["account_uuid"] = json!("known-uuid");
    value["org_uuid"] = json!("organization-uuid");
    let fixture = fixture(vec![]);
    let result = import_login(LoginKind::Claude, &params(value), &fixture, &fixture)
        .await
        .unwrap();
    assert_eq!(
        result["accountIdentity"],
        json!({"issuer":"https://claude.ai","subject":"known-uuid","scope":"organization-uuid"})
    );
    assert_eq!(result["credentials"]["expires"], 1);
    assert_eq!(result["refreshed"], false);
    assert!(fixture.requests.lock().unwrap().is_empty());
    assert_eq!(fixture.saves.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn claude_email_without_uuid_stays_in_a_separate_unverified_namespace() {
    let fixture = fixture(vec![]);
    let result = import_login(LoginKind::Claude, &params(claude()), &fixture, &fixture)
        .await
        .unwrap();
    assert_eq!(
        result["accountIdentity"]["issuer"],
        "urn:nora:import:claude-unverified"
    );
    assert_eq!(result["accountIdentity"]["subject"], "claude@example.test");
    assert!(result["accountIdentity"].get("scope").is_none());
    assert!(result["credentials"]
        .pointer("/extras/noraClaudeCLI")
        .is_none());
}
#[tokio::test]
async fn claude_known_uuid_without_organization_omits_optional_scope() {
    let fixture = fixture(vec![]);
    let mut value = claude();
    value["account_uuid"] = json!("known-uuid");
    let result = import_login(LoginKind::Claude, &params(value), &fixture, &fixture)
        .await
        .unwrap();
    assert_eq!(
        result["accountIdentity"],
        json!({"issuer":"https://claude.ai","subject":"known-uuid"})
    );
    assert!(result["credentials"]["extras"]["accountIdentity"]
        .get("scope")
        .is_none());
}
#[tokio::test]
async fn claude_export_outer_organization_aliases_keep_different_organizations_separate() {
    let mut identities = Vec::new();
    for (key, organization) in [
        ("organization_uuid", "organization-one"),
        ("organizationUuid", "organization-two"),
    ] {
        let fixture = fixture(vec![]);
        let mut value = claude();
        value["account_uuid"] = json!("same-user");
        value[key] = json!(organization);
        let result = import_login(LoginKind::Claude, &params(value), &fixture, &fixture)
            .await
            .unwrap();
        assert_eq!(result["accountIdentity"]["subject"], "same-user");
        assert_eq!(result["accountIdentity"]["scope"], organization);
        identities.push(result["accountIdentity"].clone());
    }
    assert_ne!(identities[0], identities[1]);
}
#[tokio::test]
async fn nora_claude_export_uses_uuid_identity_rather_than_the_hashed_account_id() {
    let fixture = fixture(vec![]);
    let value = json!({"type":"oauth","credentialProvider":"anthropic","accountId":"private-hash","accountIdentity":{"issuer":"https://claude.ai","subject":"known-uuid","scope":"known-org"},"email":"person@example.test","access":"private-access","refresh":"private-refresh"});
    let result = import_login(LoginKind::Claude, &params(value), &fixture, &fixture)
        .await
        .unwrap();
    assert_eq!(result["accountIdentity"]["subject"], "known-uuid");
    assert_eq!(result["accountIdentity"]["scope"], "known-org");
}
#[test]
fn manifest_account_import_declarations_match_the_implemented_formats() {
    for (kind, manifest) in [
        (
            LoginKind::Codex,
            include_str!("../../openai/nora-provider.json"),
        ),
        (
            LoginKind::Claude,
            include_str!("../../anthropic/nora-provider.json"),
        ),
    ] {
        let manifest: Value = serde_json::from_str(manifest).unwrap();
        assert_eq!(manifest["config"]["accountImport"], declaration(kind));
    }
}
#[test]
fn normal_account_renewal_keeps_private_import_metadata_and_prefers_new_identity() {
    let previous = json!({"accountIdentity":{"issuer":"https://identity.test","subject":"old"},"extras":{"noraImportFingerprint":"fingerprint","noraImportState":"complete"}});
    let mut next = json!({"access":"rotated-access","refresh":"rotated-refresh"});
    preserve_metadata(&previous, &mut next);
    assert_eq!(next["extras"]["noraImportFingerprint"], "fingerprint");
    assert_eq!(next["accountIdentity"]["subject"], "old");
    next["accountIdentity"] = json!({"issuer":"https://identity.test","subject":"fresh"});
    preserve_metadata(&previous, &mut next);
    assert_eq!(next["accountIdentity"]["subject"], "fresh");
}
#[tokio::test]
async fn vendor_failures_and_display_fields_never_return_secrets() {
    let denied = fixture(vec![]);
    denied.replies.lock().unwrap().push_back((
        401,
        json!({"error":"old-private-refresh new-private-refresh"}),
    ));
    let error = import_login(LoginKind::Codex, &params(codex()), &denied, &denied)
        .await
        .unwrap_err();
    assert!(!error.message.contains("private"));
    let fixture = fixture(vec![
        json!({"access_token":jwt(json!({"name":"new-private-refresh","sub":"person","https://api.openai.com/auth":{"chatgpt_account_id":"workspace"}})),"refresh_token":"new-private-refresh","expires_in":3600}),
    ]);
    let result = import_login(LoginKind::Codex, &params(codex()), &fixture, &fixture)
        .await
        .unwrap();
    assert!(!result["profile"].to_string().contains("private-refresh"));
}
