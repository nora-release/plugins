use super::*;
#[test]
fn models_choose_only_an_advertised_protocol_and_preserve_native_ids() {
    let models=remote(&json!({"data":[{"id":"custom/native","display_name":"Native","context_length":32768,"modalities":{"input":["text","image"]},"supported_endpoints":["/v1/responses"]}]}),"Max",false).unwrap();
    assert_eq!(models[0]["id"], "custom/native");
    assert_eq!(models[0]["api"], "openai-responses");
    assert_eq!(models[0]["contextWindow"], 32768);
    assert!(models[0]["input"]
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v == "image"));
}
#[test]
fn plan_entitlements_follow_the_actual_cli_table_and_extra_credits_open_access() {
    assert!(refuses("Go", "claude-opus-5-5"));
    assert!(refuses("Pro", "gpt-6-astra"));
    assert!(!refuses("GOAT", "gpt-6-luna"));
    let value = json!({"data":[{"id":"claude-opus-5-5"},{"id":"gpt-6-luna"}]});
    assert_eq!(remote(&value, "Go", false).unwrap().len(), 1);
    assert_eq!(remote(&value, "Go", true).unwrap().len(), 2);
}
#[test]
fn go_models_always_use_the_generate_chat_path_with_exact_reasoning_levels() {
    let models = presets("Go", false);
    let deepseek = models
        .iter()
        .find(|m| m["id"] == "deepseek/deepseek-v4-pro")
        .unwrap();
    assert_eq!(deepseek["api"], "openai-completions");
    assert_eq!(deepseek["info"]["reasoningEfforts"], json!(["high", "max"]));
    assert_eq!(deepseek["info"]["catalogSource"], "preset");
}
