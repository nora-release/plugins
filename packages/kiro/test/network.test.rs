use super::*;
#[path = "fixture.rs"]
mod fixture;
#[tokio::test]
async fn models_and_usage_follow_profile_region_and_the_real_management_query() {
    let client = fixture::Client::new(vec![
        fixture::Reply::json(json!({})),
        fixture::Reply::json(fixture::usage()),
    ]);
    let c = Credential {
        access: "private-token".into(),
        profile: fixture::PROFILE.into(),
        region: "us-east-1".into(),
        ..Default::default()
    };
    management(&client, &c, "List-Available-Models", None)
        .await
        .unwrap();
    management(&client, &c, "Get-Usage-Limits", None)
        .await
        .unwrap();
    let captured = client.requests.lock().unwrap();
    assert!(captured[0]
        .url
        .starts_with("https://management.eu-central-1.kiro.dev/List-Available-Models?"));
    let parsed = url::Url::parse(&captured[1].url).unwrap();
    let query = parsed
        .query_pairs()
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(query["origin"], "KIRO_CLI");
    assert_eq!(query["resourceType"], "CREDIT");
    assert_eq!(query["isEmailRequired"], "true");
    assert_eq!(query["profileArn"], fixture::PROFILE);
    assert_eq!(captured[0].headers["authorization"], "Bearer private-token");
}
#[tokio::test]
async fn builder_profile_fallback_requires_the_specific_builder_error() {
    let c = Credential {
        method: "idc".into(),
        access: "access".into(),
        ..Default::default()
    };
    let client = fixture::Client::new(vec![fixture::Reply::raw(
        403,
        b"AWS Builder ID is not supported for this operation".to_vec(),
    )]);
    assert_eq!(
        profile(&client, &c).await.unwrap(),
        session::BUILDER_PROFILE
    );
    let client = fixture::Client::new(vec![
        fixture::Reply::raw(403, b"private-token permission denied".to_vec()),
        fixture::Reply::json(json!({"profiles":[{"arn":fixture::PROFILE}]})),
    ]);
    assert_eq!(profile(&client, &c).await.unwrap(), fixture::PROFILE);
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn api_keys_resolve_their_own_profile_with_api_key_token_type() {
    let client = fixture::Client::new(vec![fixture::Reply::json(
        json!({"profile":{"arn":fixture::PROFILE}}),
    )]);
    let c = Credential::api_key(&json!({"apiKey":"ksk_private"})).unwrap();
    assert_eq!(profile(&client, &c).await.unwrap(), fixture::PROFILE);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].headers["tokentype"], "API_KEY");
    assert_eq!(
        requests[0].headers["x-amz-target"],
        "AmazonCodeWhispererService.GetProfile"
    );
    assert_eq!(requests[0].url, "https://management.us-east-1.kiro.dev/");
}
#[tokio::test]
async fn social_and_idc_refresh_use_the_login_region_and_private_renewal_fields() {
    for method in ["social", "idc"] {
        let client = fixture::Client::new(vec![fixture::Reply::json(
            json!({"accessToken":"next","refreshToken":"rotated","expiresIn":3600}),
        )]);
        let c = Credential {
            method: method.into(),
            access: "old".into(),
            refresh: "refresh".into(),
            region: "eu-west-1".into(),
            client_id: "client".into(),
            client_secret: "secret".into(),
            ..Default::default()
        };
        let next = refresh(&client, &c).await.unwrap();
        assert_eq!(next.refresh, "rotated");
        assert!(next.fresh());
        let requests = client.requests.lock().unwrap();
        let body: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
        if method == "social" {
            assert_eq!(
                requests[0].url,
                "https://prod.eu-west-1.auth.desktop.kiro.dev/refreshToken"
            );
            assert_eq!(body["refreshToken"], "refresh");
        } else {
            assert_eq!(
                requests[0].url,
                "https://oidc.eu-west-1.amazonaws.com/token"
            );
            assert_eq!(body["clientSecret"], "secret");
            assert_eq!(body["grantType"], "refresh_token");
        }
    }
}
#[tokio::test]
async fn metadata_errors_and_oversized_responses_never_echo_private_content() {
    for reply in [
        fixture::Reply::raw(500, b"private-token".to_vec()),
        fixture::Reply::raw(200, b"private-token".to_vec()),
        fixture::Reply::raw(200, vec![b' '; 4 * 1024 * 1024 + 1]),
    ] {
        let client = fixture::Client::new(vec![reply]);
        let error = json(
            &client,
            HttpRequest::new(HttpMethod::Get, "https://management.us-east-1.kiro.dev/"),
        )
        .await
        .unwrap_err();
        assert!(!error.message.contains("private-token"));
    }
}
#[tokio::test]
async fn missing_refresh_rotation_keeps_the_existing_refresh_token() {
    let client = fixture::Client::new(vec![fixture::Reply::json(
        json!({"accessToken":"next","expiresIn":3600}),
    )]);
    let previous = Credential {
        method: "social".into(),
        refresh: "existing".into(),
        region: "us-east-1".into(),
        ..Default::default()
    };
    assert_eq!(
        refresh(&client, &previous).await.unwrap().refresh,
        "existing"
    );
}
