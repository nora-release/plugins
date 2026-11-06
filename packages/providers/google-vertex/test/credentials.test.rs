use super::*;
#[path = "support.rs"]
mod support;

#[tokio::test]
async fn person_tokens_use_the_fixed_google_endpoint_and_do_not_expose_refusals() {
    let dir = support::Directory::new();
    let path = dir.write(&support::person());
    let config = support::config(path.clone());
    let before = std::fs::read(&path).unwrap();
    let client = support::Client::new(vec![json!({"access_token":"access-one","expires_in":3600})]);
    let token = mint(&client, &read(&config).unwrap(), &config)
        .await
        .unwrap();
    assert_eq!(token.access, "access-one");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].url, TOKEN);
    let form = url::form_urlencoded::parse(requests[0].body.as_ref().unwrap())
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(form["refresh_token"], "fixture-refresh");
    assert_eq!(form["client_id"], "fixture-client");
    assert_eq!(std::fs::read(&path).unwrap(), before);
    drop(requests);
    for status in [400, 401, 403, 429, 500] {
        let bad = support::Client::raw(status, b"private-credential-reflected-in-error".to_vec());
        let error = mint(&bad, &read(&config).unwrap(), &config)
            .await
            .err()
            .unwrap();
        assert!(!error.message.contains("private-credential"));
        assert!(error.message.contains(&status.to_string()));
    }
}

#[tokio::test]
async fn service_account_jwt_is_signed_for_google_with_bounded_lifetime() {
    use rsa::pkcs8::{EncodePrivateKey, EncodePublicKey, LineEnding};
    let key = rsa::RsaPrivateKey::new(&mut rand::thread_rng(), 2048).unwrap();
    let private = key.to_pkcs8_pem(LineEnding::LF).unwrap();
    let public = key
        .to_public_key()
        .to_public_key_pem(LineEnding::LF)
        .unwrap();
    let dir = support::Directory::new();
    let path=dir.write(&json!({"type":"service_account","client_email":"account@project.iam.gserviceaccount.com","private_key":private.as_str(),"private_key_id":"kid-one","token_uri":"https://evil.example/steal"}));
    let config = support::config(path);
    let client = support::Client::new(vec![
        json!({"access_token":"service-access","expires_in":3600}),
    ]);
    mint(&client, &read(&config).unwrap(), &config)
        .await
        .unwrap();
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, TOKEN);
    let form = url::form_urlencoded::parse(requests[0].body.as_ref().unwrap())
        .collect::<std::collections::BTreeMap<_, _>>();
    let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
    validation.set_audience(&[TOKEN]);
    let decoded = jsonwebtoken::decode::<Value>(
        &form["assertion"],
        &jsonwebtoken::DecodingKey::from_rsa_pem(public.as_bytes()).unwrap(),
        &validation,
    )
    .unwrap();
    assert_eq!(decoded.header.kid.as_deref(), Some("kid-one"));
    assert_eq!(
        decoded.claims["iss"],
        "account@project.iam.gserviceaccount.com"
    );
    assert_eq!(decoded.claims["scope"], SCOPE);
    assert_eq!(
        decoded.claims["exp"].as_i64().unwrap() - decoded.claims["iat"].as_i64().unwrap(),
        3600
    );
}

#[tokio::test]
async fn impersonation_uses_google_iam_instead_of_a_host_named_by_the_source_file() {
    let dir = support::Directory::new();
    let path=dir.write(&json!({"type":"impersonated_service_account","source_credentials":support::person(),"service_account_impersonation_url":"https://evil.example/v1/projects/-/serviceAccounts/target%40project.iam.gserviceaccount.com:generateAccessToken","delegates":["delegate@project.iam.gserviceaccount.com"]}));
    let config = support::config(path);
    let client = support::Client::new(vec![
        json!({"access_token":"source-access","expires_in":3600}),
        json!({"accessToken":"impersonated-access","expireTime":"2099-01-01T00:00:00Z"}),
    ]);
    let token = mint(&client, &read(&config).unwrap(), &config)
        .await
        .unwrap();
    assert_eq!(token.access, "impersonated-access");
    let requests = client.requests.lock().unwrap();
    assert!(requests[1].url.starts_with(IAM));
    assert!(!requests[1].url.contains("evil.example"));
    assert_eq!(requests[1].headers["authorization"], "Bearer source-access");
    let body: Value = serde_json::from_slice(requests[1].body.as_ref().unwrap()).unwrap();
    assert_eq!(
        body["delegates"][0],
        "projects/-/serviceAccounts/delegate@project.iam.gserviceaccount.com"
    );
}

#[test]
fn unsupported_sources_and_recursive_impersonation_never_mint_another_identity() {
    let dir = support::Directory::new();
    for value in [
        json!({"type":"external_account"}),
        json!({"type":"authorized_user","client_id":"one","refresh_token":"r","universe_domain":"evil.example"}),
        json!({"type":"impersonated_service_account","service_account_impersonation_url":"https://iamcredentials.googleapis.com/v1/projects/-/serviceAccounts/user@example.com:generateAccessToken","source_credentials":{"type":"impersonated_service_account"}}),
    ] {
        let path = dir.write(&value);
        let config =
            Config::from_inputs(&json!({"project":"project-one","sourcePath":path})).unwrap();
        assert!(read(&config).is_err());
    }
}
