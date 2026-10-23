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
async fn api_key_profile_lookup_tries_the_two_cli_regions_before_accepting_a_profile() {
    let client = fixture::Client::new(vec![
        fixture::Reply::raw(400, b"No CodeWhisperer profile".to_vec()),
        fixture::Reply::json(json!({"profile":{"arn":fixture::PROFILE}})),
    ]);
    let credential = Credential::api_key(&json!({"apiKey":"ksk_private"})).unwrap();
    assert_eq!(
        profile(&client, &credential).await.unwrap(),
        fixture::PROFILE
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, "https://management.us-east-1.kiro.dev/");
    assert_eq!(requests[1].url, "https://management.eu-central-1.kiro.dev/");
    for request in requests.iter() {
        assert_eq!(
            request.headers["x-amz-target"],
            "AmazonCodeWhispererService.GetProfile"
        );
        assert_eq!(
            request.headers["content-type"],
            "application/x-amz-json-1.0"
        );
        assert_eq!(request.headers["x-amzn-codewhisperer-optout"], "false");
        assert_eq!(request.headers["tokentype"], "API_KEY");
        assert_eq!(request.body.as_ref().unwrap(), b"{}");
    }
}

#[tokio::test]
async fn an_api_key_without_a_codewhisperer_profile_or_email_still_has_a_key_identity() {
    let client = fixture::Client::new(vec![
        fixture::Reply::raw(400, b"No profile".to_vec()),
        fixture::Reply::raw(400, b"No profile".to_vec()),
        fixture::Reply::json(json!({"usageBreakdownList":[]})),
    ]);
    let mut credential = Credential::api_key(&json!({"apiKey":"ksk_private"})).unwrap();
    identify(&client, &mut credential).await.unwrap();
    assert!(credential.profile.is_empty());
    assert!(credential.email.is_empty());
    assert!(credential.user_id.is_empty());
    assert!(credential.account_identity().is_none());
    assert_eq!(credential.identity, key_identity("ksk_private"));
    assert_ne!(credential.identity, key_identity("ksk_another_private"));
    assert_eq!(credential.saved()["type"], "api");
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests[2].url,
        "https://management.us-east-1.kiro.dev/?origin=KIRO_CLI"
    );
    assert_eq!(
        requests[2].headers["x-amz-target"],
        "AmazonCodeWhispererService.GetUsageLimits"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(requests[2].body.as_ref().unwrap()).unwrap(),
        json!({"origin":"KIRO_CLI"})
    );
    assert!(!requests[2].url.contains("profileArn"));
    assert!(!requests[2].url.contains("isEmailRequired"));
}

#[tokio::test]
async fn api_key_usage_uses_the_profile_region_with_no_profile_or_email_in_its_control_request() {
    let client = fixture::Client::new(vec![fixture::Reply::json(fixture::usage())]);
    let credential = Credential {
        method: "apikey".into(),
        access: "private-key".into(),
        profile: fixture::PROFILE.into(),
        region: "us-east-1".into(),
        ..Default::default()
    };
    management(&client, &credential, "Get-Usage-Limits", None)
        .await
        .unwrap();
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].url,
        "https://management.eu-central-1.kiro.dev/?origin=KIRO_CLI"
    );
    assert_eq!(
        requests[0].headers["content-type"],
        "application/x-amz-json-1.0"
    );
    assert_eq!(
        requests[0].headers["x-amz-target"],
        "AmazonCodeWhispererService.GetUsageLimits"
    );
    assert_eq!(
        serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap(),
        json!({"origin":"KIRO_CLI"})
    );
}

#[tokio::test]
async fn api_key_models_follow_control_plane_pages_and_keep_the_first_available_default() {
    let client = fixture::Client::new(vec![
        fixture::Reply::json(
            json!({"models":[{"modelId":"first"}],"defaultModel":{"modelId":"preferred"},"nextToken":"next+page"}),
        ),
        fixture::Reply::json(
            json!({"models":[{"modelId":"preferred"}],"defaultModel":{"modelId":"first"}}),
        ),
    ]);
    let credential = Credential {
        method: "apikey".into(),
        access: "private-key".into(),
        profile: fixture::PROFILE.into(),
        ..Default::default()
    };
    let result = management(&client, &credential, "List-Available-Models", None)
        .await
        .unwrap();
    assert_eq!(result["models"].as_array().unwrap().len(), 2);
    assert_eq!(result["defaultModel"]["modelId"], "preferred");
    assert_eq!(
        crate::usage::models(&result).unwrap()["models"][0]["id"],
        "preferred"
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    for request in requests.iter() {
        assert_eq!(
            request.url,
            "https://management.eu-central-1.kiro.dev/?origin=KIRO_CLI"
        );
        assert_eq!(
            request.headers["x-amz-target"],
            "KiroControlPlaneBearerService.ListAvailableModels"
        );
        assert_eq!(request.headers["tokentype"], "API_KEY");
    }
    assert_eq!(
        serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap(),
        json!({"origin":"KIRO_CLI"})
    );
    assert_eq!(
        serde_json::from_slice::<Value>(requests[1].body.as_ref().unwrap()).unwrap(),
        json!({"origin":"KIRO_CLI","nextToken":"next+page"})
    );
}

#[tokio::test]
async fn api_key_model_pagination_is_limited_to_twenty_pages() {
    let client=fixture::Client::new((0..20).map(|index|fixture::Reply::json(json!({"models":[{"modelId":format!("model-{index}")}],"nextToken":format!("page-{}",index+1)}))).collect());
    let credential = Credential::api_key(&json!({"apiKey":"ksk_private"})).unwrap();
    let result = management(&client, &credential, "List-Available-Models", None)
        .await
        .unwrap();
    assert_eq!(result["models"].as_array().unwrap().len(), 20);
    assert_eq!(client.requests.lock().unwrap().len(), 20);
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
async fn external_idp_refresh_uses_its_https_form_endpoint_and_rotated_snake_case_fields() {
    let client = fixture::Client::new(vec![fixture::Reply::json(
        json!({"access_token":"external-next","refresh_token":"external-rotation","expires_in":1800}),
    )]);
    let credential = Credential {
        method: "external-idp".into(),
        refresh: "private refresh+value".into(),
        client_id: "client+id".into(),
        token_endpoint: "https://login.example.test/oauth/token?tenant=organization".into(),
        realm: "https://issuer.example.test/organization".into(),
        ..Default::default()
    };
    let next = refresh(&client, &credential).await.unwrap();
    assert_eq!(next.access, "external-next");
    assert_eq!(next.refresh, "external-rotation");
    assert!(next.fresh());
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].url, credential.token_endpoint);
    assert_eq!(
        requests[0].headers["content-type"],
        "application/x-www-form-urlencoded"
    );
    let fields = url::form_urlencoded::parse(requests[0].body.as_ref().unwrap())
        .collect::<std::collections::BTreeMap<_, _>>();
    assert_eq!(fields["grant_type"], "refresh_token");
    assert_eq!(fields["client_id"], "client+id");
    assert_eq!(fields["refresh_token"], "private refresh+value");
}

#[tokio::test]
async fn external_idp_rejects_missing_registration_or_unsafe_token_endpoints_before_http() {
    for endpoint in [
        "http://login.example.test/token",
        "https://private:secret@login.example.test/token",
        "https://login.example.test/token#fragment",
    ] {
        let client = fixture::Client::new(vec![]);
        let credential = Credential {
            method: "external-idp".into(),
            refresh: "private-refresh".into(),
            client_id: "client".into(),
            token_endpoint: endpoint.into(),
            ..Default::default()
        };
        assert!(refresh(&client, &credential).await.is_err());
        assert!(client.requests.lock().unwrap().is_empty());
    }
    let client = fixture::Client::new(vec![]);
    let credential = Credential {
        method: "external-idp".into(),
        refresh: "private-refresh".into(),
        token_endpoint: "https://login.example.test/token".into(),
        ..Default::default()
    };
    assert!(refresh(&client, &credential).await.is_err());
    assert!(client.requests.lock().unwrap().is_empty());
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

fn identified_credential() -> Credential {
    Credential {
        access: "private-access".into(),
        method: "social".into(),
        region: "us-east-1".into(),
        profile: fixture::PROFILE.into(),
        ..Default::default()
    }
}

#[tokio::test]
async fn service_user_id_takes_precedence_over_email_and_survives_email_changes() {
    let mut credential = identified_credential();
    let client = fixture::Client::new(vec![
        fixture::Reply::json(
            json!({"userInfo":{"userId":"service-user","email":"first@example.test"}}),
        ),
        fixture::Reply::json(
            json!({"userInfo":{"userId":"service-user","email":"second@example.test"}}),
        ),
    ]);
    identify(&client, &mut credential).await.unwrap();
    let first = credential.saved();
    assert_eq!(first["accountIdentity"]["subject"], "user:service-user");
    identify(&client, &mut credential).await.unwrap();
    assert_eq!(credential.email, "second@example.test");
    assert_eq!(
        credential.saved()["accountIdentity"],
        first["accountIdentity"]
    );
    assert!(credential.matches_saved_identity(&first));
}

#[tokio::test]
async fn verified_email_fallback_is_scoped_to_the_billing_profile() {
    let mut first = identified_credential();
    let client = fixture::Client::new(vec![
        fixture::Reply::json(fixture::usage()),
        fixture::Reply::json(fixture::usage()),
    ]);
    identify(&client, &mut first).await.unwrap();
    assert_eq!(
        first.account_identity().unwrap()["subject"],
        "email:person@example.test"
    );
    let mut second = identified_credential();
    second.profile.push_str("-other-billing-profile");
    identify(&client, &mut second).await.unwrap();
    assert_ne!(first.account_identity(), second.account_identity());
    assert!(!second.matches_saved_identity(&first.saved()));
}

#[tokio::test]
async fn different_users_of_the_shared_builder_profile_do_not_merge_without_email() {
    let client = fixture::Client::new(vec![
        fixture::Reply::json(json!({"userInfo":{"userId":"one"}})),
        fixture::Reply::json(json!({"userInfo":{"userId":"two"}})),
        fixture::Reply::json(json!({"userInfo":{}})),
    ]);
    let mut credential = Credential {
        builder: true,
        profile: session::BUILDER_PROFILE.into(),
        ..identified_credential()
    };
    identify(&client, &mut credential).await.unwrap();
    let first = credential.saved();
    assert_eq!(first["accountIdentity"]["subject"], "user:one");
    identify(&client, &mut credential).await.unwrap();
    assert!(!credential.matches_saved_identity(&first));
    identify(&client, &mut credential).await.unwrap();
    assert!(credential.account_identity().is_none());
}

#[tokio::test]
async fn invalid_service_identity_is_rejected_without_exposing_its_value() {
    for user_id in [
        json!("private invalid id"),
        json!(true),
        json!("\nprivate-value"),
        json!("x".repeat(501)),
    ] {
        let client = fixture::Client::new(vec![fixture::Reply::json(
            json!({"userInfo":{"userId":user_id,"email":"person@example.test"}}),
        )]);
        let error = identify(&client, &mut identified_credential())
            .await
            .err()
            .unwrap();
        assert!(!error.message.contains("private"));
    }
}

#[tokio::test]
async fn rotating_a_token_keeps_verified_user_and_enterprise_scope_until_rechecked() {
    let client = fixture::Client::new(vec![fixture::Reply::json(
        json!({"accessToken":"next","expiresIn":3600}),
    )]);
    let credential = Credential {
        method: "idc".into(),
        client_id: "client".into(),
        client_secret: "secret".into(),
        refresh: "private-refresh".into(),
        user_id: "service-user".into(),
        realm: "https://tenant.awsapps.com/start".into(),
        ..identified_credential()
    };
    let next = refresh(&client, &credential).await.unwrap();
    assert_eq!(next.account_identity(), credential.account_identity());
    assert_eq!(next.realm, credential.realm);
    assert_eq!(next.user_id, credential.user_id);
}
