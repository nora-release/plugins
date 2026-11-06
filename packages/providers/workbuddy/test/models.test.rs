use super::*;

#[test]
fn live_cli_models_keep_region_specific_images_efforts_and_whole_day_prices() {
    let config = json!({"agents":[{"name":"cli","models":["glm-5.3","free-model"]}],"models":[{"id":"glm-5.3","name":"GLM","maxInputTokens":1000000,"maxOutputTokens":32000,"supportsImages":true,"supportsReasoning":true,"reasoning":{"effort":"medium"},"credits":"x2.00"},{"id":"free-model","name":"Free","supportsImages":true,"credits":"x0.00"}]});
    let china = parse(Site::China, &config, now()).unwrap();
    let international = parse(Site::International, &config, now()).unwrap();
    assert_eq!(china["models"][0]["input"], json!(["text", "image"]));
    assert_eq!(international["models"][0]["input"], json!(["text"]));
    assert_eq!(
        china["models"][0]["info"]["reasoningEfforts"],
        json!(["none", "low", "medium", "high", "max"])
    );
    assert_eq!(china["models"][1]["info"]["free"], true);
    assert_eq!(china["models"][0]["info"]["creditMultiplier"], 2.0);
    assert_eq!(china["models"][1]["info"]["creditMultiplier"], 0.0);
    assert!(china["models"][0].get("creditMultiplier").is_none());
    assert_eq!(china["source"], "remote");
    assert_eq!(china["allowUnknownModels"], false);
}

#[test]
fn an_hourly_free_promotion_never_marks_a_model_free_for_the_whole_day() {
    let model = json!({"id":"m","credits":"x0.00"});
    let nightly = json!([{"kind":"discount","modelIds":["m"],"discount":{"discountedCredits":"0.00x"},"schedule":{"daily":[{"from":"23:00","to":"08:00"}]}}]);
    assert_eq!(price(&model, &nightly, now()), (None, None, Some(false)));
    let model = json!({"id":"m","credits":"x2"});
    let daily =
        json!([{"kind":"discount","modelIds":["m"],"discount":{"discountedCredits":"0.50x"}}]);
    assert_eq!(
        price(&model, &daily, now()),
        (Some(0.5), Some(2.0), Some(false))
    );
    assert!(parse(Site::China, &json!({"agents":[],"models":[]}), now()).is_err());
}
