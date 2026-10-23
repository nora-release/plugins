use super::*;
use crate::tests::{configured, data, Client};

fn credentials() -> Value {
    json!({"type":"oauth","access":"fixture-access","uid":"member","expires":now()+3_600_000})
}

fn catalog() -> Value {
    json!({"agents":[{"name":"cli","models":["free","paid","night"]}],"models":[{"id":"free","credits":"x0.00"},{"id":"paid","credits":"x1.00"},{"id":"night","credits":"x0.50"}],"modelPromotions":[{"kind":"discount","modelIds":["night"],"discount":{"discountedCredits":"0.00x"},"schedule":{"daily":[{"start":"23:00","end":"08:00"}]}}]})
}

fn spent() -> Value {
    json!({"IsPaidUser":false,"Packages":[{"PackageCode":"paid","CycleTotalCapacity":"560","CycleUsedCapacity":"560"}]})
}

#[test]
fn credit_windows_preserve_amounts_and_enterprise_beijing_reset_time() {
    let usage=parse(&json!({"IsPaidUser":true,"Packages":[{"CycleTotalCapacity":"100","CycleUsedCapacity":"42.25"}]}),"").unwrap();
    assert_eq!(usage["plan"], "Pro");
    assert_eq!(usage["windows"][0]["amount"], 42.25);
    assert_eq!(usage["windows"][0]["limit"], 100.0);
    let enterprise=enterprise_window(&json!({"limitNum":"500","credit":"125","cycleStartTime":"2026-10-01 00:00:00","cycleResetTime":"2026-11-01 00:00:00"})).unwrap().unwrap();
    assert_eq!(enterprise["resetsAt"], "2026-10-31T16:00:00.000Z");
    assert_eq!(enterprise["span"], 31 * 86400);
    assert_eq!(enterprise["used"], 25.0);
}

#[test]
fn earliest_counted_non_daily_credits_are_the_only_expiring_allowance() {
    let at = models::date("2026-10-22T00:00:00Z").unwrap();
    let summary = json!({"Packages":[{"PackageCode":"paid"},{"PackageCode":DAILY}]});
    let packages = vec![
        json!({"PackageCode":DAILY,"CycleCapacityRemain":"1000","DeductionEndTime":at+1000}),
        json!({"PackageCode":"other","CycleCapacityRemain":"500","DeductionEndTime":at+1000}),
        json!({"PackageCode":"paid","CycleCapacityRemainPrecise":"22.25","ExpiredTime":"2026-10-23 08:00:00"}),
        json!({"PackageCode":"paid","CycleCapacityRemain":"10","ExpiredTime":"2026-10-23 08:00:00"}),
    ];
    assert_eq!(
        runs_out(&summary, &packages, at).unwrap(),
        Some(("2026-10-23T00:00:00.000Z".into(), 32.25))
    );
    assert!(capacity(Some(&json!("not-a-number"))).is_err());
}

#[tokio::test]
async fn empty_personal_allowances_keep_their_card_when_the_enterprise_meter_refuses_them() {
    let client = Client::new(vec![
        data(json!({"IsPaidUser":false,"Packages":[]})),
        (
            400,
            json!({"code":10001,"msg":"uid or enterpriseID is empty"}),
        ),
        data(catalog()),
    ]);
    let provider = configured(Site::China, client.clone());
    let result = fetch(&provider, client.as_ref(), &credentials())
        .await
        .unwrap();
    assert_eq!(result, json!({"plan":"Free","windows":[],"signIn":"kept"}));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[1]
        .url
        .ends_with("/billing/meter/get-enterprise-user-usage"));
    assert!(!requests[1].headers.contains_key("x-enterprise-id"));
}

#[tokio::test]
async fn enterprise_allowances_use_the_selected_organization_and_exclude_only_always_free_models() {
    let client = Client::new(vec![
        data(json!({"Packages":[]})),
        data(
            json!({"credit":196.13,"limitNum":2000,"cycleStartTime":"2026-10-08 00:00:00","cycleResetTime":"2026-11-09 00:00:00"}),
        ),
        data(catalog()),
    ]);
    let provider = configured(Site::China, client.clone());
    let mut account = credentials();
    account["enterpriseId"] = json!("enterprise-one");
    account["domain"] = json!("www.workbuddy.cn");
    let result = fetch(&provider, client.as_ref(), &account).await.unwrap();
    assert_eq!(result["plan"], "Enterprise");
    let window = &result["windows"][0];
    assert_eq!(window["amount"], 196.13);
    assert_eq!(window["limit"], 2000.0);
    assert_eq!(window["resetsAt"], "2026-11-08T16:00:00.000Z");
    assert_eq!(window["span"], 32 * 86400);
    assert_eq!(window["notModels"], json!(["free"]));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[1].method, HttpMethod::Post);
    assert_eq!(requests[1].body.as_deref(), Some(b"{}".as_slice()));
    for (key, expected) in [
        ("authorization", "Bearer fixture-access"),
        ("x-user-id", "member"),
        ("x-domain", "www.workbuddy.cn"),
        ("x-enterprise-id", "enterprise-one"),
    ] {
        assert_eq!(
            requests[1].headers.get(key).map(String::as_str),
            Some(expected)
        );
    }
}

#[tokio::test]
async fn exhausted_personal_credits_never_consult_enterprise_billing_and_keep_free_models_usable() {
    for site in [Site::China, Site::International] {
        let client = Client::new(vec![
            data(spent()),
            (503, json!({"code":1,"msg":"package details unavailable"})),
            data(catalog()),
        ]);
        let provider = configured(site, client.clone());
        let result = fetch(&provider, client.as_ref(), &credentials())
            .await
            .unwrap();
        assert_eq!(result["windows"][0]["used"], 100.0);
        assert_eq!(result["windows"][0]["notModels"], json!(["free"]));
        assert!(result["windows"][0].get("runsOutAt").is_none());
        let requests = client.requests.lock().unwrap();
        assert!(requests
            .iter()
            .all(|request| request.url.starts_with(site.endpoint())));
        assert!(requests
            .iter()
            .all(|request| !request.url.contains("enterprise")));
    }
}

#[tokio::test]
async fn unavailable_models_leave_the_known_credit_limit_in_place_without_guessing_free_models() {
    let client = Client::new(vec![
        data(spent()),
        (503, json!({"code":1})),
        (503, json!({"code":1})),
    ]);
    let provider = configured(Site::China, client.clone());
    let result = fetch(&provider, client.as_ref(), &credentials())
        .await
        .unwrap();
    assert_eq!(result["windows"][0]["amount"], 560.0);
    assert_eq!(result["windows"][0]["used"], 100.0);
    assert!(result["windows"][0].get("notModels").is_none());
    assert_eq!(result["signIn"], "kept");
}

#[tokio::test]
async fn malformed_optional_package_details_do_not_erase_the_known_credit_allowance() {
    let client = Client::new(vec![
        data(spent()),
        data(
            json!({"Accounts":[{"PackageCode":"paid","CycleCapacityRemain":"unreadable","DeductionEndTime":now()+3600_000}]}),
        ),
        data(json!({"Accounts":[]})),
        data(catalog()),
    ]);
    let provider = configured(Site::China, client.clone());
    let result = fetch(&provider, client.as_ref(), &credentials())
        .await
        .unwrap();
    assert_eq!(result["windows"][0]["amount"], 560.0);
    assert_eq!(result["windows"][0]["notModels"], json!(["free"]));
    assert!(result["windows"][0].get("runsOutAt").is_none());
}

#[tokio::test]
async fn malformed_optional_enterprise_data_leaves_the_original_empty_card_intact() {
    let client = Client::new(vec![
        data(json!({"Packages":[]})),
        data(json!({"limitNum":"unreadable","credit":2})),
        data(catalog()),
    ]);
    let provider = configured(Site::China, client.clone());
    let result = fetch(&provider, client.as_ref(), &credentials())
        .await
        .unwrap();
    assert_eq!(result, json!({"plan":"Free","windows":[],"signIn":"kept"}));
}

#[test]
fn enterprise_limits_require_a_real_total_and_do_not_invent_reset_times() {
    for value in [Value::Null, json!({"credit":3,"limitNum":0})] {
        assert!(enterprise_window(&value).unwrap().is_none());
    }
    let window =
        enterprise_window(&json!({"credit":"12.5","limitNum":"100","cycleResetTime":"soon"}))
            .unwrap()
            .unwrap();
    assert_eq!(window["used"], 12.5);
    assert!(window.get("resetsAt").is_none());
    assert!(window.get("span").is_none());
}
