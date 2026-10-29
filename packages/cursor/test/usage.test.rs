use super::*;
fn period() -> Value {
    json!({"billingCycleEnd":"1900000000000","autoBucketModels":["composer-2"],"planUsage":{"autoPercentUsed":25,"apiPercentUsed":100,"totalPercentUsed":80},"spendLimitUsage":{"individualUsed":1200}})
}
#[test]
fn chat_pools_are_scoped_to_exact_account_models_and_grok_bot_is_independent() {
    let value = parse(
        &period(),
        Some(&json!({"noUsageBasedAllowed":true})),
        &[
            "auto".into(),
            "grok-4.7-high-fast@256k".into(),
            "claude@1m".into(),
        ],
    )
    .unwrap();
    assert_eq!(value["items"][0]["countsForRouting"], true);
    assert!(value["items"][0]["modelIDs"]
        .as_array()
        .unwrap()
        .contains(&json!("grok-4.7-high-fast")));
    assert_eq!(
        value["items"][1]["modelIDs"],
        json!(["claude", "claude@1m"])
    );
    assert_eq!(value["items"][2]["countsForRouting"], false);
    assert_eq!(
        pool_base("cursor-grok-4.7-extra-high-fast@256k"),
        "grok-4.7"
    );
}
#[test]
fn on_demand_keeps_exhausted_included_pools_from_disabling_chat() {
    let value = parse(
        &period(),
        Some(&json!({"hardLimit":100})),
        &["claude".into()],
    )
    .unwrap();
    assert_eq!(value["items"][0]["countsForRouting"], false);
    let demand = value["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["id"] == "on-demand")
        .unwrap();
    assert_eq!(demand["countsForRouting"], true);
    assert_eq!(demand["usedPercent"], 12.0);
    let value = parse(&period(), Some(&json!({"hardLimit":2147483647u64})), &[]).unwrap();
    assert!(value["items"]
        .as_array()
        .unwrap()
        .iter()
        .all(|v| v["countsForRouting"] == false));
}
#[test]
fn team_pool_is_not_the_individual_members_spend() {
    let mut data = period();
    data["spendLimitUsage"] =
        json!({"limitType":"team","individualUsed":1200,"pooledLimit":"50000","pooledUsed":49000});
    assert_eq!(
        ondemand(&data, Some(&json!({"hardLimit":100}))),
        OnDemand::Fixed(490.0, 500.0, Some(12.0))
    );
    data["spendLimitUsage"]["individualLimit"] = json!(0);
    assert_eq!(
        ondemand(&data, Some(&json!({"hardLimit":100}))),
        OnDemand::Disabled
    );
}
#[test]
fn missing_totals_are_not_fabricated_and_unknown_other_models_do_not_gate_every_model() {
    let value = parse(&json!({}), None, &[]).unwrap();
    assert_eq!(value["available"], false);
    let value = parse(&period(), None, &[]).unwrap();
    assert_eq!(value["items"][1]["countsForRouting"], false);
    assert!(parse(&Value::Null, None, &[]).is_err());
}
