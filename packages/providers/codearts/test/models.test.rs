use super::*;
use crate::provider::tests::{configured, credentials, Client};

#[tokio::test]
async fn live_models_keep_dated_free_ids_and_merge_the_agent_rates_and_limits() {
    let client = Client::new(vec![
        (
            200,
            json!({"result":{"models":[{"model_id":"deepseek-v4-flash-0731"},{"model_id":"kimi-k2.6-vl"}]}}),
        ),
        (200, json!({"error_code":"0000"})),
        (
            200,
            json!({"builtinModels":[{"model_id":"deepseek-v4-flash"},{"model_id":"GLM-5.2"}]}),
        ),
        (
            200,
            json!({"agents":[{"agent_id":"main","original_id":"build"}]}),
        ),
        (
            200,
            json!({"gpts":{"models":[{"model_parameters":{"model_id":"GLM-5.2","context_window":202752,"max_tokens":131072,"thinking_type":"enabled"},"credit":[{"ratio_display":"1.50"}]},{"model_name":"GLM 5.3","model_parameters":{"model_id":"glm-5.3"},"credit":{"ratio_display":0.8}},{"model_type":"custom","model_parameters":{"model_id":"private-model"}}]}}),
        ),
    ]);
    let provider = configured(client.clone());
    let credentials = credentials();
    let result = fetch(&provider, client.as_ref(), &credentials)
        .await
        .unwrap();
    let models = result["models"]
        .as_array()
        .unwrap()
        .iter()
        .map(|model| (text(model, "id"), model))
        .collect::<BTreeMap<_, _>>();
    assert_eq!(models.len(), 4);
    assert_eq!(models["deepseek-v4-flash-0731"]["info"]["free"], true);
    assert_eq!(models["deepseek-v4-flash"]["info"]["free"], false);
    assert_eq!(models["GLM-5.2"]["info"]["creditMultiplier"], 1.5);
    assert_eq!(models["GLM-5.2"]["maxTokens"], 65536);
    assert_eq!(models["GLM-5.2"]["reasoning"], true);
    assert_eq!(models["glm-5.3"]["info"]["creditMultiplier"], 0.8);
    assert!(!models.contains_key("private-model"));
    assert!(!models.contains_key("kimi-k2.6-vl"));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 5);
    assert!(requests[1].headers["authorization"].contains("SDK-HMAC-SHA256"));
    assert!(requests[4].url.ends_with("agent_id=main"));
}

#[tokio::test]
async fn an_unclaimed_benefit_is_never_advertised_as_a_free_model() {
    let client = Client::new(vec![
        (200, json!({"result":{"models":[{"model_id":"glm-5.3"}]}})),
        (403, json!({"error_code":"denied"})),
        (200, json!({"builtinModels":[{"model_id":"glm-5.3"}]})),
        (200, json!({"agents":[]})),
        (200, json!({"gpts":{"models":[]}})),
    ]);
    let provider = configured(client.clone());
    let credentials = credentials();
    let result = fetch(&provider, client.as_ref(), &credentials)
        .await
        .unwrap();
    assert_eq!(result["models"][0]["info"]["free"], false);
    let mut other = credentials.clone();
    other["session_id"] = json!("different-account");
    let a = provider.account(&credentials).await.unwrap();
    let b = provider.account(&other).await.unwrap();
    assert!(!std::sync::Arc::ptr_eq(&a, &b));
    assert!(b.models.lock().await.benefit.is_none());
}

#[test]
fn credit_display_is_a_multiplier_and_accepts_numeric_and_zero_values() {
    assert_eq!(credit_rate(Some(&json!({"ratio_display":0.3}))), Some(0.3));
    assert_eq!(
        credit_rate(Some(&json!([{ "ratio_display":"0" }]))),
        Some(0.0)
    );
    assert_eq!(
        credit_rate(Some(&json!({"ratio_display":"1.5x"}))),
        Some(1.5)
    );
    assert_eq!(credit_rate(Some(&json!({"ratio_display":"unknown"}))), None);
    assert_eq!(normalize("deepseek-v4-flash-0731"), "deepseek-v4-flash");
    assert_eq!(normalize("glm-5.3"), "glm-5.3");
}

#[test]
fn cold_start_billing_defaults_never_override_a_live_account_model_list() {
    let mut state = State::default();
    assert!(benefit_for(&state, "deepseek-v4-flash-0731"));
    assert!(!benefit_for(&state, "deepseek-v4-flash"));
    state.benefit = Some(BTreeSet::new());
    assert!(!benefit_for(&state, "deepseek-v4-flash-0731"));
    state
        .benefit
        .as_mut()
        .unwrap()
        .insert("new-free-model".into());
    assert!(benefit_for(&state, "new-free-model"));
    state.refused.insert("new-free-model".into());
    assert!(!benefit_for(&state, "new-free-model"));
}
