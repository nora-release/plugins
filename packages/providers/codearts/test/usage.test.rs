use super::*;
use crate::provider::tests::{configured, credentials, data, Client};

#[test]
fn usage_keeps_total_and_separate_daily_pool_and_actual_credit_expiration() {
    let at = date(&json!("2026-10-22T12:00:00+08:00")).unwrap();
    let expires = date(&json!("2026-10-24T08:00:00+08:00")).unwrap();
    let batches = vec![
        Batch {
            metric: BASIC.into(),
            amount: 100.0,
            left: 50.0,
            expires: at + 20 * 86_400_000,
        },
        Batch {
            metric: BONUS.into(),
            amount: 20.0,
            left: 15.0,
            expires,
        },
        Batch {
            metric: BONUS.into(),
            amount: 10.0,
            left: 5.0,
            expires: expires + 3_600_000,
        },
    ];
    let result=parse(&json!({"code":0,"data":{"package":{"package_name_cn":"专业版"},"metrics":[{"name":BASIC,"package_credit_amount":100,"package_credit_remain":50},{"name":TOTAL,"package_credit_amount":130,"package_credit_remain":70},{"name":BONUS,"package_credit_amount":30,"package_credit_remain":20}]}}),Some(&json!({"result":{"daily_token_limit":1000,"daily_tokens_used":250}})),&batches,&["free-only".into()],at).unwrap();
    let windows = result["windows"].as_array().unwrap();
    assert_eq!(windows[0]["name"], "总积分");
    assert_eq!(windows[0]["aside"], false);
    assert_eq!(windows[0]["runsOut"], 20.0);
    assert_eq!(windows[0]["runsOutAt"], stamp(expires));
    assert_eq!(windows[1]["aside"], true);
    let free = windows.last().unwrap();
    assert_eq!(free["models"], json!(["free-only"]));
    assert_eq!(free["used"], 25.0);
    assert_eq!(free["resetsAt"], "2026-10-22T16:00:00.000Z");
    assert_eq!(free["span"], 86400);
    assert!(parse(&json!({"metrics":[]}), None, &[], &[], at).is_err());
    assert_eq!(
        date(&json!("2026-10-22 00:00:00")),
        date(&json!("2026-10-22T00:00:00+08:00"))
    );
    assert_eq!(date(&json!(u64::MAX)), None);
}

#[tokio::test]
async fn checkin_claims_and_confirms_the_actual_campaign_then_keeps_a_private_account_ledger() {
    let client = Client::new(vec![
        data(
            json!({"items":[{"type":"USER_LOGIN","campaignId":"daily","claimable":true,"benefitAmount":5}]}),
        ),
        data(json!({"id":"grant","benefitAmount":6,"expireAt":"2099-01-01T00:00:00Z"})),
        data(json!({})),
    ]);
    let provider = configured(client.clone());
    let credentials = credentials();
    let result = checkin(&provider, client.as_ref(), &credentials)
        .await
        .unwrap();
    assert_eq!(result["outcome"], "claimed");
    assert_eq!(result["credit"], 6.0);
    assert_eq!(read_grants(&provider, &credentials).len(), 1);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 3);
    assert!(requests[1].url.ends_with("/v1/ops/claim"));
    assert!(requests[2].url.ends_with("/v1/ops/confirm"));
    assert_eq!(
        serde_json::from_slice::<Value>(requests[1].body.as_ref().unwrap()).unwrap(),
        json!({"campaignId":"daily","channel":"IDE"})
    );
    drop(requests);
    std::fs::remove_dir_all(provider.data().unwrap()).unwrap();
}

#[tokio::test]
async fn a_failed_checkin_never_records_or_reports_a_successful_grant() {
    let client = Client::new(vec![
        data(json!({"items":[{"type":"USER_LOGIN","campaignId":"daily","claimable":true}]})),
        (500, json!({"message":"declined"})),
        data(json!({"items":[]})),
    ]);
    let provider = configured(client.clone());
    let credentials = credentials();
    assert!(checkin(&provider, client.as_ref(), &credentials)
        .await
        .is_err());
    assert!(read_grants(&provider, &credentials).is_empty());
}
