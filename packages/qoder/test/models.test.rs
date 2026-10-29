use super::*;
#[test]
fn app_catalog_wins_and_retained_chat_models_keep_their_predecessor_order() {
    let entries=listing(&json!({"app":[{"key":"fast","display_name":"Fast","enable":true,"price_factor":0.8},{"key":"other","enable":true}],"chat":[{"key":"fast","display_name":"Old fast","enable":true},{"key":"performance","enable":true},{"key":"other","enable":true},{"key":"auto","enable":true},{"key":"disabled","enable":false}]})).unwrap();
    assert_eq!(
        entries
            .iter()
            .map(|entry| entry.key.as_str())
            .collect::<Vec<_>>(),
        vec!["fast", "performance", "other"]
    );
    assert_eq!(entries[0].name, "Fast");
}
#[test]
fn active_promotions_preserve_billing_multiplier_instead_of_claiming_free() {
    let entries=listing(&json!({"app":[{"key":"discounted","enable":true,"price_factor":0,"promotion":{"active":true,"before_promotion_price_factor":2,"discount_factor":0.25}},{"key":"free","enable":true,"price_factor":0,"original_price_factor":0.1}]})).unwrap();
    assert!(!entries[0].free);
    assert_eq!(entries[0].rate, Some(0.5));
    assert_eq!(entries[0].previous, Some(2.0));
    assert!(entries[1].free);
    let value = entries[0]
        .model(
            Site::International,
            &Site::International.deployment(None).unwrap(),
        )
        .unwrap();
    assert_eq!(value["info"]["creditMultiplier"], 0.5);
    assert_eq!(value["info"]["previousCreditMultiplier"], 2.0);
    assert!(value["info"].get("requestCreditRate").is_none());
}
#[test]
fn offered_context_windows_and_thinking_defaults_are_taken_from_the_live_config() {
    let entries=listing(&json!({"app":[{"key":"model","enable":true,"max_input_tokens":180000,"context_config":{"200k":{"token_count":200000,"is_default":true},"1m":{"token_count":1000000}},"thinking_config":{"enabled":{"efforts":{"low":{},"high":{"is_default":true},"max":{}}},"disabled":{}}}]})).unwrap();
    let model = &entries[0];
    assert_eq!(model.context, 1000000);
    assert_eq!(model.default_window, 200000);
    assert_eq!(effort("", model), (true, "high".into()));
    assert_eq!(effort("none", model), (false, String::new()));
    assert_eq!(effort("ultra", model), (true, "max".into()));
    assert_eq!(effort("medium", model), (true, "high".into()));
    let mut forced = model.clone();
    forced.always_thinks = true;
    assert_eq!(effort("none", &forced), (true, "low".into()));
}
#[test]
fn a_failed_or_empty_catalog_never_succeeds_with_stale_presets() {
    assert!(listing(&json!({"app":[]})).is_err());
    assert!(listing(&Value::Null).is_err());
}
