use super::*;

#[tokio::test]
async fn vendor_cost_words_are_relative_credit_rates_not_dollar_prices() {
    for (cost, multiplier) in [
        ("low", 0.5),
        ("medium", 1.0),
        ("high", 2.0),
        ("very_high", 4.0),
    ] {
        let (plugin, _) = setup(vec![(
            200,
            json!({"data":[{"id":"new-model","metadata":{"cost":cost}}]}),
        )]);
        let listing = plugin.models(context()).await.unwrap();
        let model = &listing["models"][0];
        assert_eq!(model["info"]["creditMultiplier"].as_f64(), Some(multiplier));
        assert_eq!(model["info"]["pricingKnown"], false);
        assert_eq!(model["cost"], json!({"input":0,"output":0}));
        assert!(model["info"].get("free").is_none());
        assert!(model["info"].get("requestCreditRate").is_none());
        assert!(model.get("providerOptions").is_none());
    }
}

#[tokio::test]
async fn seed_rates_match_the_published_catalog_but_a_live_rate_replaces_them() {
    let (plugin, _) = setup(vec![]);
    let listing = plugin.models(CallContext::default()).await.unwrap();
    for (id, multiplier) in [
        ("gpt-6-luna", 0.5),
        ("mimo-v2.6-pro", 1.0),
        ("deepseek-v4-pro", 2.0),
    ] {
        let model = listing["models"]
            .as_array()
            .unwrap()
            .iter()
            .find(|model| model["id"] == id)
            .unwrap();
        assert_eq!(model["info"]["creditMultiplier"].as_f64(), Some(multiplier));
    }
    let (plugin, _) = setup(vec![(
        200,
        json!({"data":[{"id":"gpt-6-luna","metadata":{"cost":"very_high"}}]}),
    )]);
    let listing = plugin.models(context()).await.unwrap();
    assert_eq!(
        listing["models"][0]["info"]["creditMultiplier"].as_f64(),
        Some(4.0)
    );
}

#[tokio::test]
async fn absent_or_unknown_live_prices_never_reuse_a_seed_rate_or_claim_free_use() {
    for metadata in [
        json!({}),
        json!({"cost":null}),
        json!({"cost":"future"}),
        json!({"cost":0}),
        json!({"cost":"0"}),
    ] {
        let (plugin, _) = setup(vec![(
            200,
            json!({"data":[{"id":"gpt-6-luna","metadata":metadata}]}),
        )]);
        let listing = plugin.models(context()).await.unwrap();
        let info = &listing["models"][0]["info"];
        assert!(info.get("creditMultiplier").is_none(), "{metadata}");
        assert_ne!(info["pricingKnown"], true);
        assert_ne!(info["free"], true);
    }
}
