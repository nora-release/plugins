use super::*;
#[test]
fn personal_wallets_preserve_known_balance_and_pools_without_inventing_percentages() {
    let context = json!({"data":{"plan":{"name":"Free"},"quota":{"total":null,"used":null,"remaining":12.345}}});
    let wallets = json!({"data":{"daily_credits":{"total_balance":2},"monthly_credits":{"totalBalance":4.5},"longterm_credits":{"total_balance":0}}});
    let value = parse(&context, Some(&wallets));
    assert_eq!(value["plan"], "Free");
    assert!(value["items"].as_array().unwrap().is_empty());
    assert_eq!(value["balances"][0]["amount"], 12.345);
    assert_eq!(
        value["balances"][0]["value"],
        "12.35 credits (daily 2 · monthly 4.5)"
    );
}
#[test]
fn known_totals_and_spent_balances_are_routed_from_actual_quota() {
    let value = parse(&json!({"quota":{"total":100,"remaining":25}}), None);
    assert_eq!(value["items"][0]["usedPercent"], 75.0);
    for quota in [
        json!({"remaining":0}),
        json!({"remaining":3,"isQuotaExceeded":true}),
    ] {
        let value = parse(&json!({"quota":quota}), None);
        assert_eq!(value["items"][0]["usedPercent"], 100);
    }
    let value = parse(&json!({"quota":{"total":"100","remaining":"20"}}), None);
    assert_eq!(value["available"], false);
    assert_eq!(parse(&json!({}), None)["available"], false);
}
