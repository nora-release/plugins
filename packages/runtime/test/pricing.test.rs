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
