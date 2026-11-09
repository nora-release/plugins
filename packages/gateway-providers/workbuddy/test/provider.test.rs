use super::*;
use std::collections::VecDeque;
use std::sync::Mutex as StdMutex;

/// Stands in for WorkBuddy: answers each request with the next reply and
/// keeps the requests.
pub(super) struct Client {
    pub requests: StdMutex<Vec<HttpRequest>>,
    pub replies: StdMutex<VecDeque<(u16, Value)>>,
}
impl Client {
    pub fn new(replies: Vec<(u16, Value)>) -> Arc<Self> {
        Arc::new(Self {
            requests: StdMutex::new(Vec::new()),
            replies: StdMutex::new(replies.into()),
        })
    }
}
#[async_trait::async_trait]
impl HttpClient for Client {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request);
        let (status, value) = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected WorkBuddy network request");
        let (bytes, kind) = if let Some(raw) = value.get("raw").and_then(Value::as_str) {
            (raw.as_bytes().to_vec(), "text/event-stream")
        } else {
            (value.to_string().into_bytes(), "application/json")
        };
        let body: HttpByteStream = Box::pin(futures::stream::once(async move { Ok(bytes) }));
        Ok(HttpStreamingResponse {
            status,
            headers: BTreeMap::from([("content-type".into(), kind.into())]),
            body,
        })
    }
}
pub(super) fn configured(site: Site, client: Arc<Client>) -> WorkBuddy {
    let mut provider = WorkBuddy::new(site);
    provider.client = Some(client);
    provider
}
pub(super) fn data(value: Value) -> (u16, Value) {
    (200, json!({"code":0,"data":value}))
}

/// A browser sign-in opens WorkBuddy's page, then each look either waits or
/// ends in the account: the token kept from one look to the next.
#[tokio::test]
async fn a_browser_sign_in_opens_the_page_then_waits_until_the_token_and_account_are_there() {
    for site in [Site::China, Site::International] {
        let client = Client::new(vec![
            data(json!({"state":"sign-in-state","authUrl":"https://login.tencent.com/auth"})),
            (200, json!({"code":11217,"msg":"waiting"})),
            data(json!({"accessToken":"access","refreshToken":"refresh","expiresIn":3600})),
            (200, json!({"code":12151,"msg":"waiting"})),
            data(json!({"uid":"user-1","nickname":"Member","type":"personal"})),
        ]);
        let provider = configured(site, client.clone());
        let Step::Open { url, session, .. } =
            auth::sign_in(&provider, client.as_ref()).await.unwrap()
        else {
            panic!("no page to open");
        };
        assert!(url.contains("version=2.0.0"));
        assert_eq!(
            auth::finish(&provider, client.as_ref(), &session)
                .await
                .unwrap(),
            Step::Waiting
        );
        assert_eq!(
            auth::finish(&provider, client.as_ref(), &session)
                .await
                .unwrap(),
            Step::Waiting
        );
        let Step::Account(account) = auth::finish(&provider, client.as_ref(), &session)
            .await
            .unwrap()
        else {
            panic!("not signed in");
        };
        assert_eq!(account.name, "Member");
        assert_eq!(account.credentials["refresh"], "refresh");
        assert!(account.expires_at.is_some());
        assert!(auth::finish(&provider, client.as_ref(), &session)
            .await
            .is_err());
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 5);
        assert!(requests
            .iter()
            .all(|request| request.url.starts_with(site.endpoint())));
        assert!(!requests[2].headers.contains_key("authorization"));
        assert_eq!(
            requests[4].headers.get("authorization").map(String::as_str),
            Some("Bearer access")
        );
    }
}

#[test]
fn each_provider_names_its_site_and_another_is_refused() {
    let plugin = WorkBuddyPlugin::default();
    let question = |provider: &str| Question {
        provider: provider.into(),
        ..Question::default()
    };
    assert_eq!(
        plugin.site(&question("workbuddy")).unwrap().site,
        Site::China
    );
    assert_eq!(
        plugin.site(&question("workbuddy-ai")).unwrap().site,
        Site::International
    );
    assert!(plugin.site(&question("codebuddy")).is_err());
}

#[test]
fn usage_and_checkin_answers_read_as_the_router_reads_them() {
    let usage = usage_of(&json!({"plan":"Pro","windows":[
        {"name":"Credits","used":25.0,"amount":250,"limit":1000,"unit":"credits","resetsAt":"2026-11-08T16:00:00.000Z"}
    ]}));
    assert_eq!(usage.plan.as_deref(), Some("Pro"));
    let window = &usage.windows[0];
    assert_eq!((window.used, window.balance), (Some(25.0), Some(750.0)));
    assert_eq!(
        window.resets_at.as_deref(),
        Some("2026-11-08T16:00:00.000Z")
    );
    assert_eq!(
        checkin_of(&json!({"outcome":"claimed","credit":5})).outcome,
        CheckinOutcome::Claimed
    );
    assert_eq!(
        checkin_of(&json!({"outcome":"claimed","credit":5})).credit,
        Some(5.0)
    );
    for other in ["own-app", "ineligible", "inactive", "ready"] {
        assert_eq!(
            checkin_of(&json!({"outcome":other})).outcome,
            CheckinOutcome::Unavailable
        );
    }
}

/// A sign-in that needs the account again tells the router so.
#[test]
fn a_refused_sign_in_reads_as_signing_in_again() {
    let error: RpcError = signed_out().into();
    assert_eq!(
        error.code,
        nora_plugin_runtime_sdk::provider::SIGN_IN_NEEDED
    );
}
