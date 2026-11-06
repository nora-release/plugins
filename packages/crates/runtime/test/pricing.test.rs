use super::*;
use serde_json::json;

fn model() -> Model {
    Model::new("m", "openai-completions", "p")
}

#[test]
fn missing_prices_do_not_make_an_unpriced_model_free() {
    let mut model = model();
    apply(&json!({"id":"m"}), &mut model);
    assert_eq!(model.info.free, None);
    assert_eq!(model.info.pricing_known, None);
    apply(&json!({"free":true}), &mut model);
    assert_eq!(model.info.free, Some(true));
    assert!(model.provider_options.is_empty());
}

#[test]
fn remote_per_token_prices_and_catalog_per_million_prices_use_one_unit() {
    let mut remote = model();
    apply(
        &json!({"pricing":{"prompt":"0.000003","completion":"0.000015","input_cache_read":"0.0000003"}}),
        &mut remote,
    );
    assert_eq!(remote.cost.input, 3.0);
    assert_eq!(remote.cost.output, 15.0);
    assert!((remote.cost.cache_read - 0.3).abs() < 1e-9);
    assert_eq!(remote.info.pricing_known, Some(true));
    let mut catalog = model();
    apply(
        &json!({"cost":{"input":3,"output":15,"cache_read":0.3}}),
        &mut catalog,
    );
    assert_eq!(catalog.cost, remote.cost);
}

#[test]
fn zero_prices_need_both_token_sides_and_non_token_charges_are_not_free() {
    for (value, known, free) in [
        (json!({"cost":{"input":0,"output":0}}), true, None),
        (json!({"pricing":{"prompt":"0"}}), false, None),
        (
            json!({"pricing":{"prompt":"0","completion":"0","request":"0.01"}}),
            false,
            Some(false),
        ),
        (
            json!({"pricing":{"prompt":"NaN","completion":"-1"}}),
            false,
            None,
        ),
    ] {
        let mut model = model();
        apply(&value, &mut model);
        assert_eq!(model.info.pricing_known, Some(known));
        assert_eq!(model.info.free, free);
    }
}

#[test]
fn a_vendor_can_explicitly_mark_free_or_paid_without_request_options() {
    for (key, value) in [("free", true), ("is_free", false), ("isFree", true)] {
        let mut model = model();
        apply(&json!({key:value}), &mut model);
        assert_eq!(model.info.free, Some(value));
        assert!(model.provider_options.is_empty());
    }
}

#[test]
fn context_tiers_inherit_base_parts_and_sort_by_actual_threshold() {
    let mut model = model();
    apply(
        &json!({"cost":{"input":5,"output":25,"cache_read":0.5,"cache_write":6.25,"tiers":[
            {"tier":{"type":"context","size":272000},"input":10,"output":37.5},
            {"above":128000,"cache_read":1},
            {"tier":{"type":"requests","size":10},"input":100},
            {"above":0,"input":100},
            {"tier":{"type":"context"},"input":100}
        ]}}),
        &mut model,
    );
    assert_eq!(model.info.pricing_known, Some(true));
    assert_eq!(model.info.pricing_tiers.len(), 2);
    let lower = &model.info.pricing_tiers[0];
    assert_eq!(lower.above, 128000);
    assert_eq!(lower.input.as_f64(), Some(5.0));
    assert_eq!(lower.output.as_f64(), Some(25.0));
    assert_eq!(lower.cache_read.as_f64(), Some(1.0));
    assert_eq!(lower.cache_write.as_f64(), Some(6.25));
    assert!(model.info.pricing_tiers[1].cache_write_1h.is_none());
}

#[test]
fn claude_hour_defaults_only_apply_when_the_base_hour_price_is_missing() {
    let price = json!({"cost":{"input":5,"output":25,"cache_write":6.25,"tiers":[
        {"above":1000,"input":10}, {"above":2000,"input":15,"cache_write_1h":35}
    ]}});
    let mut claude = Model::new("vendor/Claude-current", "anthropic-messages", "relay");
    apply(&price, &mut claude);
    assert_eq!(
        claude
            .info
            .cache_write_1h
            .as_ref()
            .and_then(serde_json::Number::as_f64),
        Some(10.0)
    );
    assert_eq!(
        claude.info.pricing_tiers[0]
            .cache_write_1h
            .as_ref()
            .and_then(serde_json::Number::as_f64),
        Some(20.0)
    );
    assert_eq!(
        claude.info.pricing_tiers[1]
            .cache_write_1h
            .as_ref()
            .and_then(serde_json::Number::as_f64),
        Some(35.0)
    );
    let mut explicit = price;
    explicit["cost"]["cache_write_1h"] = json!(11);
    apply(&explicit, &mut claude);
    assert_eq!(
        claude
            .info
            .cache_write_1h
            .as_ref()
            .and_then(serde_json::Number::as_f64),
        Some(11.0)
    );
    assert!(claude.info.pricing_tiers[0].cache_write_1h.is_none());
    let mut other = model();
    apply(&explicit, &mut other);
    assert!(other.info.pricing_tiers[0].cache_write_1h.is_none());
    apply(
        &json!({"cost":{"input":5,"output":25,"cache_write":0}}),
        &mut claude,
    );
    assert!(claude.info.cache_write_1h.is_none());
}

#[test]
fn non_claude_cache_writes_never_receive_an_invented_hour_price() {
    let mut model = model();
    apply(
        &json!({"cost":{"input":10,"output":50,"cache_write":12.5,"tiers":[{"above":272000,"input":20}]}}),
        &mut model,
    );
    assert!(model.info.cache_write_1h.is_none());
    assert!(model.info.pricing_tiers[0].cache_write_1h.is_none());
}

#[test]
fn per_token_tiers_and_explicit_hour_prices_use_the_same_per_million_unit() {
    let mut model = model();
    apply(
        &json!({"pricing":{"prompt":"0.000003","completion":"0.000015","input_cache_write_1h":"0.000006","tiers":[{"above":1000,"prompt":"0.000005","input_cache_write_1h":"0.00001"}]}}),
        &mut model,
    );
    assert_eq!(model.cost.input, 3.0);
    assert_eq!(
        model
            .info
            .cache_write_1h
            .as_ref()
            .and_then(serde_json::Number::as_f64),
        Some(6.0)
    );
    assert_eq!(model.info.pricing_tiers[0].input.as_f64(), Some(5.0));
    assert_eq!(
        model.info.pricing_tiers[0]
            .cache_write_1h
            .as_ref()
            .and_then(serde_json::Number::as_f64),
        Some(10.0)
    );
}

#[test]
fn unreasonable_prices_or_tiers_are_unknown_and_do_not_replace_a_valid_quote() {
    for invalid in [
        json!({"input":-1,"output":1}),
        json!({"input":"NaN","output":1}),
        json!({"input":1000001,"output":1}),
        json!({"input":{},"output":1}),
        json!({"input":1,"output":1,"cache_write_1h":-1}),
        json!({"input":1,"output":1,"tiers":[{"above":10,"input":-1}]}),
        json!({"input":1,"output":1,"tiers":[{"above":10},{"above":10}]}),
        json!({"input":1,"output":1,"tiers":"not-an-array"}),
    ] {
        let mut model = model();
        model.cost.input = 9.0;
        apply(&json!({"cost":invalid}), &mut model);
        assert_eq!(model.cost.input, 9.0);
        assert_eq!(model.info.pricing_known, Some(false));
    }
}

#[test]
fn relay_price_metadata_stays_out_of_provider_request_options() {
    let mut model = model();
    apply(
        &json!({"magpie_price":{"input":1,"output":2,"cache_write_1h":3,"tiers":[{"above":1000,"input":2}]}}),
        &mut model,
    );
    let encoded = serde_json::to_value(&model).unwrap();
    assert_eq!(encoded["info"]["cacheWrite1h"], 3.0);
    assert_eq!(encoded["info"]["pricingTiers"][0]["above"], 1000);
    assert!(model.provider_options.is_empty());
}

#[test]
fn actual_paid_context_tiers_override_free_flags_even_when_the_base_price_is_zero() {
    for marked in [Value::Null, json!(true), json!(false)] {
        let mut model = model();
        apply(
            &json!({"free":marked,"cost":{"input":0,"output":0,"tiers":[{"above":1000,"input":2,"output":3}]}}),
            &mut model,
        );
        assert_eq!(model.cost.input, 0.0);
        assert_eq!(model.info.free, Some(false));
        assert_eq!(model.info.pricing_known, Some(true));
    }
}

#[test]
fn too_many_context_tiers_are_not_accepted_as_a_complete_quote() {
    let mut model = model();
    apply(
        &json!({"cost":{"input":1,"output":2,"tiers":(1..=65).map(|above| json!({"above":above})).collect::<Vec<_>>()}}),
        &mut model,
    );
    assert_eq!(model.info.pricing_known, Some(false));
    assert!(model.info.pricing_tiers.is_empty());
}
