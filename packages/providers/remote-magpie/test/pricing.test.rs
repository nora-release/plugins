use super::*;
fn model(price: Value, id: &str) -> Value {
    let mut model = json!({"info":{}});
    apply(&mut model, &json!({"magpie_price":price}), id);
    model
}
#[test]
fn declared_prices_and_whole_request_tiers_keep_the_remotes_units() {
    let value = model(
        json!({"input":3,"output":12,"cache_read":0.3,"cache_write":3.75,"tiers":[{"above":200000,"input":6,"output":18,"cache_read":0.6,"cache_write":7.5}]}),
        "a/model",
    );
    assert_eq!(
        value["cost"],
        json!({"input":3.0,"output":12.0,"cacheRead":0.3,"cacheWrite":3.75})
    );
    assert_eq!(value["info"]["pricingTiers"][0]["above"], 200000);
    assert_eq!(value["info"]["pricingTiers"][0]["output"], 18.0);
    assert_eq!(value["info"]["pricingKnown"], true);
    assert_eq!(value["info"]["free"], false);
}
#[test]
fn explicit_zero_prices_are_known_free_while_absent_prices_stay_unknown() {
    let value = model(json!({"input":0,"output":0}), "m");
    assert_eq!(value["info"]["free"], true);
    let mut value = json!({"info":{}});
    apply(&mut value, &json!({}), "m");
    assert!(value.get("cost").is_none());
    assert!(value["info"].get("pricingKnown").is_none());
}
#[test]
fn invalid_prices_are_removed_without_inventing_a_zero_price() {
    for price in [
        json!("cheap"),
        json!({"input":-1}),
        json!({"output":1000001}),
        json!({"input":"3"}),
        json!({"tiers":[{"above":10,"input":-1}]}),
    ] {
        let value = model(price, "m");
        assert!(value.get("cost").is_none());
    }
}
#[test]
fn tiers_inherit_base_parts_sort_by_threshold_and_skip_noncontext_or_sizeless_entries() {
    let value = model(
        json!({"input":2,"output":8,"cache_write":2.5,"tiers":[{"above":500,"output":16},{"tier":{"type":"context","size":100},"input":4},{"tier":{"type":"output","size":1},"input":99},{"above":0,"input":99}]}),
        "m",
    );
    let tiers = value["info"]["pricingTiers"].as_array().unwrap();
    assert_eq!(tiers.len(), 2);
    assert_eq!(tiers[0]["above"], 100);
    assert_eq!(tiers[0]["output"], 8.0);
    assert_eq!(tiers[1]["input"], 2.0);
}
#[test]
fn claude_hourly_defaults_follow_the_whole_price_rule_and_never_apply_to_other_models() {
    let value = model(
        json!({"input":3,"cache_write":3.75,"tiers":[{"above":200,"input":6,"cache_write":7.5}]}),
        "relay/claude-custom",
    );
    assert_eq!(value["info"]["cacheWrite1h"], 6.0);
    assert_eq!(value["info"]["pricingTiers"][0]["cacheWrite1h"], 12.0);
    let value = model(
        json!({"input":3,"cache_write":3.75,"cache_write_1h":9,"tiers":[{"above":200,"input":6}]}),
        "claude-custom",
    );
    assert_eq!(value["info"]["cacheWrite1h"], 9.0);
    assert_eq!(value["info"]["pricingTiers"][0]["cacheWrite1h"], 0.0);
    let value = model(json!({"input":3,"cache_write":3.75}), "gpt-model");
    assert_eq!(value["info"]["cacheWrite1h"], 0.0);
}
