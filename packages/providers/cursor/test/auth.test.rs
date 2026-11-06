use super::*;
use crate::fixture::{Client, Reply};
#[test]
fn jwt_expiry_and_selected_file_are_bounded_and_read_only() {
    let jwt = format!(
        "x.{}.x",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(br#"{"exp":1900000000}"#)
    );
    assert_eq!(expiry(&jwt), 1_900_000_000_000);
    assert_eq!(expiry("opaque"), 0);
    let path = std::env::temp_dir().join(format!("nora-cursor-source-{}", random()));
    let original =
        json!({"accessToken":"private-selected-token","refreshToken":"must-not-be-copied"})
            .to_string();
    std::fs::write(&path, &original).unwrap();
    assert_eq!(
        read_file(path.to_str().unwrap()).unwrap(),
        "private-selected-token"
    );
    assert_eq!(std::fs::read_to_string(&path).unwrap(), original);
    std::fs::write(&path, vec![b' '; 128 * 1024 + 1]).unwrap();
    assert!(read_file(path.to_str().unwrap()).is_err());
    std::fs::remove_file(path).unwrap();
    assert!(read_file("relative/auth.json").is_err());
}
#[tokio::test]
async fn browser_pkce_poll_reads_real_identity_and_never_sends_the_verifier_to_another_origin() {
    let client = Client::new(vec![
        Reply::json(
            json!({"accessToken":"private-browser-token","refreshToken":"private-refresh"}),
        ),
        Reply::json(json!({"email":"actual@example.test"})),
        Reply::json(json!({"planInfo":{"planName":"Ultra"}})),
    ]);
    let (pending, url) = start(client.clone(), network::FALLBACK_VERSION.into());
    let url = url::Url::parse(&url).unwrap();
    assert_eq!(url.origin().ascii_serialization(), "https://cursor.com");
    let query: BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    assert_eq!(
        query["challenge"],
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(Sha256::digest(pending.verifier.as_bytes()))
    );
    assert!(!url.as_str().contains(&pending.verifier));
    let result = pending.complete().await.unwrap();
    assert_eq!(result["accountId"], "actual@example.test");
    assert_eq!(result["plan"], "Ultra");
    let requests = client.requests.lock().unwrap();
    assert!(requests.iter().all(|r| r.url.starts_with(network::API)));
    assert_eq!(requests[0].method, nora_ai::HttpMethod::Get);
    assert!(!requests[0]
        .headers
        .values()
        .any(|s| s.contains("private-browser-token")));
}
#[tokio::test]
async fn exchange_and_poll_failures_do_not_echo_credentials_or_response_bodies() {
    let client = Client::new(vec![Reply::status(
        401,
        json!({"message":"private-api-key private-reflected-token"}),
    )]);
    let error = exchange(
        client.as_ref(),
        "private-api-key",
        network::FALLBACK_VERSION,
    )
    .await
    .unwrap_err();
    assert!(!error.message.contains("private"));
    assert_eq!(error.code, "authentication_required");
    let client = Client::new(vec![Reply::json(json!({"accessToken":"missing-refresh"}))]);
    let (pending, _) = start(client, network::FALLBACK_VERSION.into());
    assert!(pending.complete().await.is_err());
}
use std::collections::BTreeMap;
