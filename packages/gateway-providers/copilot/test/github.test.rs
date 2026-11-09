use super::*;
use crate::tests::Client;

#[tokio::test]
async fn the_device_flow_starts_with_the_editors_client() {
    let client = Client::new(vec![(
        200,
        json!({
            "device_code": "dc", "user_code": "ABCD-1234",
            "verification_uri": "https://github.com/login/device", "interval": 5, "expires_in": 900
        }),
    )]);
    let started = start(client.as_ref()).await.unwrap();
    assert_eq!(started.code, "ABCD-1234");
    assert_eq!(started.url, "https://github.com/login/device");
    assert_eq!(started.pending.interval, Duration::from_secs(5));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, DEVICE_CODE_URL);
    let body = String::from_utf8(requests[0].body.clone().unwrap()).unwrap();
    assert_eq!(body, "client_id=Iv1.b507a08c87ecfe98&scope=read%3Auser");
}

fn due() -> Pending {
    let now = Instant::now();
    Pending {
        device_code: "dc".into(),
        interval: Duration::from_secs(5),
        next: now,
        expires: now + Duration::from_secs(900),
    }
}

#[tokio::test]
async fn polling_waits_out_githubs_interval() {
    let client = Client::new(vec![(200, json!({"error": "authorization_pending"}))]);
    let mut pending = due();
    assert_eq!(poll(client.as_ref(), &mut pending).await.unwrap(), None);
    // asked again at once: GitHub is not asked
    assert_eq!(poll(client.as_ref(), &mut pending).await.unwrap(), None);
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn slow_down_lengthens_the_interval() {
    let client = Client::new(vec![(200, json!({"error": "slow_down"}))]);
    let mut pending = due();
    assert_eq!(poll(client.as_ref(), &mut pending).await.unwrap(), None);
    assert_eq!(pending.interval, Duration::from_secs(10));
}

#[tokio::test]
async fn the_code_typed_gives_the_token() {
    let client = Client::new(vec![(
        200,
        json!({"access_token": "gho_1", "token_type": "bearer"}),
    )]);
    let mut pending = due();
    let token = poll(client.as_ref(), &mut pending).await.unwrap();
    assert_eq!(token.as_deref(), Some("gho_1"));
    let body = String::from_utf8(client.requests.lock().unwrap()[0].body.clone().unwrap()).unwrap();
    assert!(body.contains("device_code=dc"));
    assert!(body.contains("grant_type=urn%3Aietf%3Aparams%3Aoauth%3Agrant-type%3Adevice_code"));
}

#[tokio::test]
async fn a_refused_sign_in_says_so() {
    let client = Client::new(vec![(200, json!({"error": "access_denied"}))]);
    let error = poll(client.as_ref(), &mut due()).await.unwrap_err();
    assert!(error.to_string().contains("cancelled"), "{error}");
}

#[tokio::test]
async fn the_account_is_named_by_its_login() {
    let client = Client::new(vec![(200, json!({"login": "octocat", "id": 1}))]);
    let account = account(client.as_ref(), "gho_1").await.unwrap();
    assert_eq!(account.name, "octocat");
    assert_eq!(
        account.credentials,
        json!({"github": "gho_1", "login": "octocat"})
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests[0].headers.get("authorization").map(String::as_str),
        Some("token gho_1")
    );
}
