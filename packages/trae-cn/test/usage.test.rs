use super::*;
use crate::tests::{configured, credentials, Client};
#[test]
fn cn_credits_and_global_dollars_keep_their_actual_units_limits_and_resets() {
    let credentials = credentials();
    let cn=parse(Site::China,&json!({"is_credits_billing":true,"user_entitlement_pack_list":[{"entitlement_base_info":{"quota":{"credits_limit":100},"end_time":"2099-01-01T00:00:00Z"},"usage":{"credits_amount":25}},{"entitlement_base_info":{"quota":{"credits_limit":50}},"usage":{"credits_amount":5}}]}),&credentials).unwrap();
    assert_eq!(cn["windows"][0]["amount"], 30.0);
    assert_eq!(cn["windows"][0]["limit"], 150.0);
    assert_eq!(cn["windows"][0]["unit"], "credits");
    let global=parse(Site::Global,&json!({"user_entitlement_pack_list":[{"display_desc":"Pro","entitlement_base_info":{"quota":{"basic_usage_limit":20,"bonus_usage_limit":5},"end_time":"2099-01-01T00:00:00Z"},"usage":{"basic_usage_amount":2,"bonus_usage_amount":1}}]}),&credentials).unwrap();
    assert_eq!(global["plan"], "Pro");
    assert_eq!(global["windows"][0]["unit"], "usd");
    assert_eq!(global["windows"][0]["amount"], 3.0);
    assert_eq!(global["windows"][0]["limit"], 25.0);
    assert!(parse(Site::Global, &json!({}), &credentials).is_err());
}
#[test]
fn legacy_devices_get_one_stable_account_specific_cn_shape() {
    let a = credentials();
    assert_eq!(ug_device(&a), "1234567890123456");
    let mut legacy = a.clone();
    legacy["deviceId"] = json!("1234567890123456789");
    let first = ug_device(&legacy);
    assert_eq!(first.len(), 16);
    assert_eq!(first, ug_device(&legacy));
    legacy["uid"] = json!("other");
    assert_ne!(first, ug_device(&legacy));
}
#[tokio::test]
async fn checkin_uses_growth_headers_and_treats_device_refusals_as_ineligible() {
    for (code, outcome) in [
        ("0", "claimed"),
        ("9074", "ineligible"),
        ("9095", "ineligible"),
    ] {
        let client = Client::new(vec![
            (200, json!({"code":0,"enable":true,"credits":1})),
            (
                200,
                json!({"code":code,"credits":2,"extra_credits":3,"message":"device status"}),
            ),
        ]);
        let provider = configured(Site::China, client.clone());
        let result = checkin(&provider, client.as_ref(), &credentials())
            .await
            .unwrap();
        assert_eq!(result["outcome"], outcome);
        if outcome == "claimed" {
            assert_eq!(result["credit"], 5.0);
        }
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            serde_json::from_slice::<Value>(requests[0].body.as_ref().unwrap()).unwrap(),
            json!({"req_source":1})
        );
        assert!(requests[0].headers["user-agent"].contains("TRAE-SOLO-CN/0.1.69"));
        assert!(!requests[0].headers.contains_key("x-cloudide-token"));
    }
}
#[tokio::test]
async fn global_has_no_fake_daily_credit_action_and_lapsed_cn_tokens_fail() {
    let client = Client::new(vec![]);
    let provider = configured(Site::Global, client.clone());
    assert_eq!(
        checkin(&provider, client.as_ref(), &credentials())
            .await
            .unwrap()["outcome"],
        "inactive"
    );
    assert!(client.requests.lock().unwrap().is_empty());
    let client = Client::new(vec![(200, json!({"code":1001,"message":"not logged in"}))]);
    let provider = configured(Site::China, client.clone());
    assert_eq!(
        checkin(&provider, client.as_ref(), &credentials())
            .await
            .unwrap_err()
            .code,
        "authentication_required"
    );
}
