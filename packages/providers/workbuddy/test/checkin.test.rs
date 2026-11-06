use super::*;
use crate::tests::{configured, data, Client};
fn credentials() -> Value {
    json!({"type":"oauth","access":"fixture-private-access","uid":"member","expires":now()+3600_000})
}
fn ready() -> Value {
    json!({"active":true,"today_checked_in":false,"streak_days":2,"daily_credit":5})
}

#[tokio::test]
async fn checkin_queries_status_then_claims_the_actual_reward_and_streak() {
    let client = Client::new(vec![
        data(ready()),
        data(json!({"credit":"7.5","streak_days":"3"})),
    ]);
    let provider = configured(Site::China, client.clone());
    assert_eq!(
        run(&provider, client.as_ref(), &credentials(), false)
            .await
            .unwrap(),
        json!({"outcome":"claimed","credit":7.5,"streak":3})
    );
    let requests = client.requests.lock().unwrap();
    assert!(requests[0]
        .url
        .ends_with("/v2/billing/meter/checkin-activity-status"));
    assert!(requests[1].url.ends_with("/v2/billing/meter/daily-checkin"));
    for request in requests.iter() {
        assert_eq!(
            request.headers["authorization"],
            "Bearer fixture-private-access"
        );
        assert_eq!(request.body.as_deref(), Some(b"{}".as_slice()));
    }
}
#[tokio::test]
async fn status_only_never_claims_and_can_confirm_a_lost_success() {
    for (panel, expected) in [
        (ready(), "ready"),
        (
            json!({"active":true,"today_checked_in":true,"today_credit":9,"streak_days":4}),
            "done",
        ),
        (json!({"active":false}), "inactive"),
    ] {
        let client = Client::new(vec![data(panel)]);
        let provider = configured(Site::China, client.clone());
        let result = run(&provider, client.as_ref(), &credentials(), true)
            .await
            .unwrap();
        assert_eq!(result["outcome"], expected);
        if expected == "ready" {
            assert_eq!(result["retrySafe"], true);
        }
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}
#[tokio::test]
async fn vendor_refusals_are_stable_results_without_exposing_private_errors() {
    for (code, outcome) in [(1001, "done"), (1002, "ineligible"), (1003, "inactive")] {
        let client = Client::new(vec![
            data(ready()),
            (200, json!({"code":code,"msg":"private-token"})),
        ]);
        let provider = configured(Site::China, client.clone());
        let result = run(&provider, client.as_ref(), &credentials(), false)
            .await
            .unwrap();
        assert_eq!(result["outcome"], outcome);
        assert!(!result.to_string().contains("private-token"));
    }
}
#[tokio::test]
async fn checkin_401_with_working_usage_requires_the_vendors_own_app() {
    for valid in [true, false] {
        let client = Client::new(vec![
            (401, json!({})),
            if valid {
                data(json!({}))
            } else {
                (401, json!({}))
            },
        ]);
        let provider = configured(Site::China, client.clone());
        assert_eq!(
            run(&provider, client.as_ref(), &credentials(), false)
                .await
                .unwrap()["outcome"],
            if valid { "own-app" } else { "failed" }
        );
        assert!(client.requests.lock().unwrap()[1]
            .url
            .ends_with("/billing/meter/get-user-resource-summary"));
    }
}

#[tokio::test]
async fn bare_html_401_is_distinguished_from_an_invalid_login() {
    let client = Client::new(vec![
        (401, json!({"raw":"<html>Unauthorized</html>"})),
        data(json!({})),
    ]);
    let provider = configured(Site::China, client.clone());
    assert_eq!(
        run(&provider, client.as_ref(), &credentials(), false)
            .await
            .unwrap()["outcome"],
        "own-app"
    );
    assert_eq!(client.requests.lock().unwrap().len(), 2);
}
#[tokio::test]
async fn malformed_claim_or_ambiguous_post_is_not_success_or_safe_to_retry() {
    for response in [data(json!({})), (502, json!({"code":1,"msg":"private"}))] {
        let client = Client::new(vec![data(ready()), response]);
        let provider = configured(Site::China, client.clone());
        let error = run(&provider, client.as_ref(), &credentials(), false)
            .await
            .unwrap_err();
        assert!(!error.message.contains("private"));
        assert_eq!(client.requests.lock().unwrap().len(), 2);
    }
}
#[tokio::test]
async fn missing_panel_and_international_site_never_claim_or_invent_an_event() {
    let client = Client::new(vec![data(json!({}))]);
    let provider = configured(Site::China, client.clone());
    assert!(run(&provider, client.as_ref(), &credentials(), false)
        .await
        .is_err());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
    let client = Client::new(vec![]);
    let provider = configured(Site::International, client.clone());
    assert_eq!(
        run(&provider, client.as_ref(), &credentials(), false)
            .await
            .unwrap()["outcome"],
        "inactive"
    );
    assert!(client.requests.lock().unwrap().is_empty());
}
