use super::*;

#[test]
fn vercel_normalizes_documented_and_saved_bases_without_double_v1() {
    for base in [
        "https://ai-gateway.vercel.sh",
        "https://ai-gateway.vercel.sh/v1",
        "https://ai-gateway.vercel.sh/typesafe",
        "https://ai-gateway.vercel.sh/typesafe/v1/",
        "https://ai-gateway.vercel.sh/typesafe/v1/systemone",
        "https://ai-gateway.vercel.sh/typesafe/v1/models",
    ] {
        assert_eq!(
            normalized(DecisionFamily::Vercel, base).unwrap(),
            "https://ai-gateway.vercel.sh/typesafe/v1"
        );
    }
    let base = normalized(
        DecisionFamily::Vercel,
        "https://gateway.example/team/typesafe/v1",
    )
    .unwrap();
    assert_eq!(vercel_root(&base).unwrap(), "https://gateway.example/team");
    assert_eq!(
        normalized(DecisionFamily::Vercel, "https://ai-gateway.vercel.sh/v4/ai").unwrap(),
        "https://ai-gateway.vercel.sh/v4/ai"
    );
}

#[test]
fn cloudflare_accepts_an_explicit_account_or_defers_to_a_token_lookup() {
    assert_eq!(
        cloudflare("https://api.cloudflare.com/client/v4").unwrap(),
        ("https://api.cloudflare.com/client/v4".into(), None)
    );
    assert_eq!(
        cloudflare("https://api.cloudflare.com/client/v4/accounts/$CLOUDFLARE_ACCOUNT_ID/ai/run")
            .unwrap()
            .1,
        None
    );
    for suffix in ["accounts/acc7", "accounts/acc7/ai/run"] {
        let (api, account) =
            cloudflare(&format!("https://api.cloudflare.com/client/v4/{suffix}")).unwrap();
        assert_eq!(account.as_deref(), Some("acc7"));
        assert_eq!(
            cloudflare_run(&api, account.as_deref().unwrap()).unwrap(),
            "https://api.cloudflare.com/client/v4/accounts/acc7/ai/run"
        );
    }
    for base in [
        "https://api.cloudflare.com/client/v4/accounts/acc7/other",
        "https://api.cloudflare.com/client/v4/accounts/acc7/ai/run/@cf/cloudflare/clef",
        "https://api.cloudflare.com/client/v3",
    ] {
        assert!(cloudflare(base).is_err());
    }
}

#[test]
fn bailian_regions_and_plan_are_explicit_and_workspace_is_required() {
    assert_eq!(
        bailian("cn-beijing", &json!({"workspaceId":"ws-one"})).unwrap(),
        "https://ws-one.cn-beijing.maas.aliyuncs.com/compatible-mode/v1"
    );
    assert_eq!(
        bailian("ap-southeast-1", &json!({"workspaceId":"ws-two"})).unwrap(),
        "https://ws-two.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1"
    );
    assert_eq!(
        bailian("token-plan", &json!({})).unwrap(),
        "https://token-plan.cn-beijing.maas.aliyuncs.com/compatible-mode/v1"
    );
    assert!(bailian("cn-beijing", &json!({})).is_err());
    assert!(bailian(
        "cn-beijing",
        &json!({"workspaceId":"ws.other.example/path"})
    )
    .is_err());
    assert!(bailian("another-region", &json!({"workspaceId":"ws"})).is_err());
}

#[test]
fn endpoint_credentials_queries_fragments_and_non_https_remote_hosts_are_rejected() {
    for base in [
        "https://user:secret@api.example/v1",
        "https://api.example/v1?key=secret",
        "https://api.example/v1#secret",
        "file:///private/token",
        "http://api.example/v1",
    ] {
        assert!(url(base).is_err(), "{base}");
    }
    assert!(url("http://127.0.0.1:8080/v1").is_ok());
    assert!(url("https://proxy.example/customer/v1").is_ok());
}
