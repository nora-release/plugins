use super::*;
#[test]
fn live_catalog_preserves_order_capabilities_efforts_and_larger_context() {
    let out=parse(&json!({"models":[{"slug":"gpt-6-sol","display_name":"GPT-6 Sol","visibility":"list","context_window":128000,"max_context_window":256000,"input_modalities":["text","image"],"supported_reasoning_levels":[{"effort":"high"},{"effort":"xhigh"}]},{"slug":"gpt-6-luna"},{"slug":"gpt-6-sol"}]}),vec![]).unwrap();
    assert_eq!(out.len(), 2);
    assert_eq!(out[0]["id"], "gpt-6-sol");
    assert_eq!(out[1]["id"], "gpt-6-luna");
    assert_eq!(out[0]["input"], json!(["text", "image"]));
    assert_eq!(out[0]["info"]["reasoningEfforts"], json!(["high", "xhigh"]));
    assert_eq!(out[0]["info"]["maxContextWindow"], 256000);
    assert_eq!(out[0]["contextWindow"], 128000);
    assert_eq!(out[1]["maxTokens"], 0);
}
#[test]
fn codex_supplement_cannot_reintroduce_hidden_duplicates_or_foreign_provider_ids() {
    let out = parse(
        &json!({"models":[{"slug":"hidden","visibility":"hide"},{"slug":"listed"}]}),
        vec![
            json!({"id":"hidden"}),
            json!({"id":"listed"}),
            json!({"id":"google/model"}),
            json!({"id":"extra","provider":"openai","api":"chatgpt-codex","contextWindow":1000}),
        ],
    )
    .unwrap();
    assert_eq!(
        out.iter()
            .map(|v| v["id"].as_str().unwrap())
            .collect::<Vec<_>>(),
        vec!["listed", "extra"]
    );
    assert_eq!(out[1]["provider"], "chatgpt-api");
    assert_eq!(out[1]["api"], "openai-responses");
    assert_eq!(out[1]["contextWindow"], 1000);
}
#[test]
fn malformed_and_empty_catalogs_fail_instead_of_hiding_cached_models() {
    for input in [
        json!(null),
        json!({"data":[]}),
        json!({"models":[]}),
        json!({"models":[{}, {"slug":""}]}),
    ] {
        assert!(parse(&input, vec![]).is_err());
    }
    assert!(parse(&json!({"models":[]}), vec![json!({"id":"from-codex"})]).is_ok());
}
