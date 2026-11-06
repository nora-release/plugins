use super::*;
use crate::tests::{configured, credentials, envelope, Client};
#[tokio::test]
async fn usage_is_read_only_separates_total_and_wallets_and_does_not_manufacture_a_limit() {
    let client = Client::new(vec![
        envelope(
            json!({"total_balance":"950","wallets":[{"display_name":"Total duplicate","balance":950},{"display_name":"Free","balance":400},{"display_name":"Paid","balance":550},{"display":false,"balance":20}]}),
        ),
        envelope(json!([{"task_id":"daily_signin","status":"pending","reward_points":400}])),
    ]);
    let provider = configured(Site::China, client.clone());
    let mut context = CallContext::default();
    context.credentials = Some(credentials(Site::China));
    let usage = provider.usage(context).await.unwrap();
    assert_eq!(usage["items"][0]["amount"], 950.0);
    assert!(usage["items"][0].get("limit").is_none());
    assert_eq!(usage["balances"].as_array().unwrap().len(), 3);
    assert_eq!(usage["items"].as_array().unwrap().len(), 4);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|r| r.method == HttpMethod::Get));
    assert!(!requests.iter().any(|r| r.url.contains("complete")));
}
#[tokio::test]
async fn checkin_status_is_read_only_and_completed_or_inactive_campaigns_never_claim() {
    for (tasks, outcome) in [
        (json!([]), "inactive"),
        (
            json!([{"task_id":"daily_signin","status":"completed","reward_points":400}]),
            "done",
        ),
        (
            json!([{"task_id":"daily_signin","status":"pending","reward_points":400}]),
            "ready",
        ),
    ] {
        let client = Client::new(vec![envelope(tasks)]);
        let provider = configured(Site::International, client.clone());
        let result = provider
            .call(
                "checkinStatus",
                json!({"credentials":credentials(Site::International)}),
            )
            .await
            .unwrap();
        assert_eq!(result["outcome"], outcome);
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}
#[tokio::test]
async fn checkin_requires_real_confirmation_and_reports_actual_reward_and_streak() {
    for (data, outcome) in [
        (
            json!({"success":true,"reward_points":400,"continuous_days":7}),
            "claimed",
        ),
        (
            json!({"already_completed":true,"reward_points":400}),
            "done",
        ),
        (json!({"success":false}), "failed"),
    ] {
        let client = Client::new(vec![
            envelope(json!([{"task_id":"daily_signin","status":"pending"}])),
            envelope(data),
        ]);
        let provider = configured(Site::China, client.clone());
        let result = provider
            .call("checkin", json!({"credentials":credentials(Site::China)}))
            .await
            .unwrap();
        assert_eq!(result["outcome"], outcome);
        if outcome == "claimed" {
            assert_eq!(result["credit"], 400.0);
            assert_eq!(result["streak"], 7);
        }
        assert_eq!(client.requests.lock().unwrap().len(), 2);
    }
}
#[tokio::test]
async fn lost_or_empty_claim_response_is_ambiguous_and_is_never_sent_to_a_second_host() {
    for reply in [envelope(json!({})), (200, json!({"offline":true}))] {
        let client = Client::new(vec![
            envelope(json!([{"task_id":"daily_signin","status":"pending"}])),
            reply,
        ]);
        let provider = configured(Site::China, client.clone());
        assert!(provider
            .call("checkin", json!({"credentials":credentials(Site::China)}))
            .await
            .is_err());
        assert_eq!(client.requests.lock().unwrap().len(), 2);
    }
}
#[tokio::test]
async fn pending_status_on_an_unknown_host_is_not_a_claimable_proof_and_invalid_balances_are_not_zero(
) {
    assert!(task_status(&[json!({"task_id":"daily_signin","status":"processing"})]).is_err());
    let client = Client::new(vec![envelope(json!({"total_balance":"unknown"}))]);
    let provider = configured(Site::International, client.clone());
    let mut context = CallContext::default();
    context.credentials = Some(credentials(Site::International));
    assert!(provider.usage(context).await.is_err());
}

#[tokio::test]
async fn wallet_curves_keep_their_supplier_identity_when_rows_reorder() {
    let free = json!({"display_name":"Free credits","public_wallet_type":"free","balance":400});
    let paid = json!({"display_name":"Paid credits","public_wallet_type":"paid","balance":550});
    let client = Client::new(vec![
        envelope(json!({"total_balance":950,"wallets":[free.clone(),paid.clone()]})),
        envelope(json!([])),
        envelope(json!({"total_balance":950,"wallets":[paid,free]})),
        envelope(json!([])),
    ]);
    let first = fetch(client.as_ref(), Site::China, &credentials(Site::China))
        .await
        .unwrap();
    let next = fetch(client.as_ref(), Site::China, &credentials(Site::China))
        .await
        .unwrap();
    assert_eq!(first["balances"][1]["id"], next["balances"][2]["id"]);
    assert_eq!(first["balances"][2]["id"], next["balances"][1]["id"]);
    assert_ne!(first["balances"][1]["id"], first["balances"][2]["id"]);
}
