use super::*;
use crate::fixture::{self, Client, Reply};
#[tokio::test]
async fn device_grant_uses_real_workos_form() {
    let client = Client::new(vec![Reply::json(
        json!({"device_code":"private-device","user_code":"ABCD","verification_uri_complete":"https://auth.factory.ai/device?user_code=ABCD","interval":3,"expires_in":300}),
    )]);
    let (pending, response) = start(client.as_ref()).await.unwrap();
    assert_eq!(pending["interval"], 3000);
    assert!(response.to_string().contains("ABCD"));
    assert!(!response.to_string().contains("private-device"));
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests[0].url,
        format!("{}/authorize/device", network::WORKOS)
    );
    let form = String::from_utf8(requests[0].body.clone().unwrap()).unwrap();
    assert!(form.contains(network::CLIENT_ID));
}
#[tokio::test]
async fn refresh_preserves_refresh_rotation_and_factory_region() {
    let mut me = fixture::me();
    me["region"] = json!("eu");
    let client = Client::new(vec![Reply::json(fixture::token()), Reply::json(me)]);
    let old = Account::parse(&fixture::saved()).unwrap();
    let next = renew(client.as_ref(), &old, "workos-new").await.unwrap();
    assert_eq!(next.refresh, "private-new-refresh");
    assert_eq!(next.region, "eu");
    let requests = client.requests.lock().unwrap();
    let form = String::from_utf8(requests[0].body.clone().unwrap()).unwrap();
    assert!(form.contains("organization_id=workos-new"));
    assert_eq!(requests[1].headers["x-factory-whoami-extended"], "true");
    assert!(!requests[1].headers.contains_key("x-client-version"));
}
#[tokio::test]
async fn workos_errors_never_reflect_credentials_or_body() {
    let client = Client::new(vec![Reply::status(
        400,
        json!({"error":"invalid_grant","error_description":"private-old-refresh private prompt"}),
    )]);
    let error = renew(
        client.as_ref(),
        &Account::parse(&fixture::saved()).unwrap(),
        "",
    )
    .await
    .err()
    .unwrap();
    assert!(!error.message.contains("private"));
}
#[tokio::test]
async fn verification_links_cannot_use_plain_http_or_url_credentials() {
    for url in [
        "http://auth.factory.ai/device",
        "https://user:private@auth.factory.ai/device",
    ] {
        let client = Client::new(vec![Reply::json(
            json!({"device_code":"private-device","user_code":"ABCD","verification_uri":url}),
        )]);
        assert!(start(client.as_ref()).await.is_err());
    }
}
