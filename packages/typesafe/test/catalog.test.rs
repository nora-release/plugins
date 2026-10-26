use super::*;

fn definition(family: DecisionFamily) -> Definition {
    let value = match family {
        DecisionFamily::Cloudflare => include_str!("../../cloudflare-jev/nora-provider.json"),
        _ => include_str!("../../vercel-jev/nora-provider.json"),
    };
    Definition::parse(serde_json::from_str(value).unwrap()).unwrap()
}

#[test]
fn a_dedicated_live_decision_list_does_not_guess_model_names() {
    let entries = json!([{"id":"entirely-custom-decider","name":"Actual Name","context_window":100000,"modalities":{"input":["text","image","pdf"]},"pricing":{"input":"0.00000004","output":"0"}},{"id":"entirely-custom-decider"}]);
    let models = models(
        entries.as_array().unwrap(),
        &definition(DecisionFamily::Vercel),
        "https://ai-gateway.vercel.sh/typesafe/v1",
        false,
    )
    .unwrap();
    assert_eq!(models.len(), 1);
    assert_eq!(models[0]["name"], "Actual Name");
    assert_eq!(models[0]["contextWindow"], 100000);
    assert!((models[0]["cost"]["input"].as_f64().unwrap() - 0.04).abs() < 0.000001);
    assert_eq!(models[0]["info"]["decision"], true);
    assert_eq!(models[0]["info"]["chat"], false);
    assert_eq!(models[0]["info"]["pricingKnown"], true);
    assert_eq!(models[0]["info"]["free"], false);
    assert_eq!(models[0]["input"], json!(["text", "image", "pdf"]));
    assert!(models[0].get("provider_options").is_none());
}

#[test]
fn cloudflare_filters_other_tasks_without_inventing_missing_remote_models() {
    let entries = json!([{"name":"@cf/meta/chat"},{"id":"opaque-cf-internal-id","name":"@cf/cloudflare/clef","context_length":70000},{"name":"typesafe/jev","input_modalities":["text"]}]);
    let values = models(
        entries.as_array().unwrap(),
        &definition(DecisionFamily::Cloudflare),
        "https://api.cloudflare.com/client/v4/accounts/acc/ai/run",
        true,
    )
    .unwrap();
    assert_eq!(values.len(), 2);
    assert_eq!(values[0]["id"], "@cf/cloudflare/clef");
    assert_eq!(values[0]["contextWindow"], 70000);
    assert_eq!(values[0]["input"], json!(["text", "image", "video"]));
    assert_eq!(values[1]["contextWindow"], 32000);
    assert!(values
        .iter()
        .all(|model| model["info"]["catalogSource"] == "remote"));
    assert!(values
        .iter()
        .all(|model| model["id"] != "@cf/cloudflare/clef-flash"));
    assert!(models(
        &[],
        &definition(DecisionFamily::Cloudflare),
        "https://api.cloudflare.com/client/v4",
        true
    )
    .unwrap()
    .is_empty());
}

#[test]
fn partial_costs_are_retained_and_unknown_prices_are_never_made_free() {
    let entries = json!([{"name":"a","cost":{"input":0.1,"output":0.2}},{"name":"unknown"},{"name":"free","cost":{"input":0,"output":0}},{"name":"bad-price","pricing":{"input":"NaN","output":"-1"}}]);
    let values = models(
        entries.as_array().unwrap(),
        &definition(DecisionFamily::Vercel),
        "https://ai-gateway.vercel.sh/typesafe/v1",
        false,
    )
    .unwrap();
    assert_eq!(values[0]["cost"], json!({"input":0.1,"output":0.2}));
    assert!(values[1]["info"].get("free").is_none());
    assert_eq!(values[2]["info"]["free"], true);
    assert!(values[3].get("cost").is_none());
}

#[test]
fn malformed_and_oversized_remote_model_lists_fail_explicitly() {
    let definition = definition(DecisionFamily::Vercel);
    for entries in [
        json!([{"name":""}]),
        json!([{"name":"bad\nmodel"}]),
        json!([{"name":12}]),
    ] {
        assert!(models(
            entries.as_array().unwrap(),
            &definition,
            "https://ai-gateway.vercel.sh/typesafe/v1",
            false
        )
        .is_err());
    }
    assert!(models(
        &vec![json!({"name":"m"}); 4097],
        &definition,
        "https://ai-gateway.vercel.sh/typesafe/v1",
        false
    )
    .is_err());
}

#[test]
fn jev_through_a_gateway_gets_the_same_missing_facts_and_keeps_vendor_values() {
    let entries = json!([
        {"id":"typesafe/jev-1.13"},
        {"id":"typesafe/jev-preview", "contextWindow":64000, "input":["text","image"]},
        {"id":"unknown-decider"}
    ]);
    let values = models(
        entries.as_array().unwrap(),
        &definition(DecisionFamily::Vercel),
        "https://ai-gateway.vercel.sh/typesafe/v1",
        false,
    )
    .unwrap();
    assert_eq!(values[0]["contextWindow"], 32000);
    assert_eq!(values[0]["input"], json!(["text"]));
    assert_eq!(values[1]["contextWindow"], 64000);
    assert_eq!(values[1]["input"], json!(["text", "image"]));
    assert!(values[2].get("contextWindow").is_none());
    assert!(values[2].get("input").is_none());
}
