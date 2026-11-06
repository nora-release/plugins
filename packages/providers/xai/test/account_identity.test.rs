use super::*;
use nora_ai::{HttpClientError, HttpRequest, HttpResponse, HttpStreamingResponse, JsonValue};

struct UserClient;
#[async_trait]
impl HttpClient for UserClient {
    async fn stream(
        &self,
        _request: HttpRequest,
    ) -> Result<HttpStreamingResponse, HttpClientError> {
        unreachable!("account verification uses /user")
    }
    async fn request(&self, request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
        assert_eq!(request.url, nora_provider_auth::XAI_OAUTH_USER_URL);
        Ok(HttpResponse { status:200, headers:BTreeMap::new(), body:serde_json::to_vec(&json!({"userId":"person-a","email":"person@example.test","principalType":"user","principalId":"person-a"})).unwrap() })
    }
}

#[tokio::test]
async fn local_and_owned_sessions_have_the_same_identity_without_matching_display_labels() {
    let mut local = OAuthCredentials::new(
        OAuthProviderId::XAI,
        "",
        None,
        None,
        BTreeMap::from([
            ("noraAccountSource".into(), json!({"kind":"grokCLI"}).into()),
            ("userId".into(), JsonValue::string("person-a")),
            ("email".into(), JsonValue::string("old@example.test")),
        ]),
    );
    let mut owned = OAuthCredentials::new(
        OAuthProviderId::XAI,
        "session",
        Some("refresh".into()),
        None,
        BTreeMap::new(),
    );
    Xai::default()
        .enrich_account(&mut local, &UserClient, None)
        .await
        .unwrap();
    Xai::default()
        .enrich_account(&mut owned, &UserClient, None)
        .await
        .unwrap();
    assert_eq!(
        local.extras["accountIdentity"],
        owned.extras["accountIdentity"]
    );
    assert_eq!(
        owned.extras["accountIdentity"].as_value()["subject"],
        "person-a"
    );
    assert_ne!(local.extras["email"], owned.extras["email"]);
    assert!(local.access_token.is_empty());
    assert_eq!(owned.access_token, "session");
}

#[tokio::test]
async fn separate_billing_organizations_are_not_merged_just_because_the_person_is_the_same() {
    let mut first = OAuthCredentials::new(
        OAuthProviderId::XAI,
        "",
        None,
        None,
        BTreeMap::from([
            ("noraAccountSource".into(), json!({"kind":"grokCLI"}).into()),
            ("userId".into(), JsonValue::string("person-a")),
            ("organizationId".into(), JsonValue::string("org-a")),
        ]),
    );
    let mut second = first.clone();
    second
        .extras
        .insert("organizationId".into(), JsonValue::string("org-b"));
    Xai::default()
        .enrich_account(&mut first, &UserClient, None)
        .await
        .unwrap();
    Xai::default()
        .enrich_account(&mut second, &UserClient, None)
        .await
        .unwrap();
    assert_ne!(
        first.extras["accountIdentity"],
        second.extras["accountIdentity"]
    );
}

#[tokio::test]
async fn an_expired_owned_or_bound_session_still_uses_its_verified_stored_user_identity() {
    struct ExpiredSession;
    #[async_trait]
    impl HttpClient for ExpiredSession {
        async fn stream(
            &self,
            _request: HttpRequest,
        ) -> Result<HttpStreamingResponse, HttpClientError> {
            panic!("identity enrichment must not send an expired token")
        }
        async fn request(&self, _request: HttpRequest) -> Result<HttpResponse, HttpClientError> {
            panic!("the stored verified userId does not need another /user request")
        }
    }
    for bound in [false, true] {
        let mut credentials = OAuthCredentials::new(
            OAuthProviderId::XAI,
            if bound { "" } else { "expired-session" },
            None,
            Some(1),
            BTreeMap::new(),
        );
        let old = if bound {
            credentials
                .extras
                .insert("noraAccountSource".into(), json!({"kind":"grokCLI"}).into());
            credentials.extras.insert(
                nora_provider_auth::grok_source::ACCOUNT_ID_KEY.into(),
                JsonValue::string("person-a"),
            );
            json!({"teamId":"team-a"})
        } else {
            json!({"userId":"person-a","teamId":"team-a"})
        };
        Xai::default().enrich_credentials(&old, &mut credentials);
        Xai::default()
            .enrich_account(&mut credentials, &ExpiredSession, None)
            .await
            .unwrap();
        assert_eq!(
            credentials.extras["accountIdentity"].as_value(),
            &json!({"issuer":"https://auth.x.ai","subject":"person-a","scope":"[\"team-a\",\"\"]"})
        );
    }
}
