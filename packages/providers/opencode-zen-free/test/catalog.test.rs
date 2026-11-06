use super::*;
#[test]
fn docs_parse_only_real_protocol_rows_and_exclude_systemone_from_chat() {
    let docs="| Name | `new-free` | `https://opencode.ai/inference/v1/responses` |\n| Jev | `jev-1.13-free` | `https://opencode.ai/inference/v1/systemone` |";
    let parsed = parse_docs(docs);
    assert_eq!(parsed["new-free"], "responses");
    let models = build(
        vec!["new-free".into(), "jev-1.13-free".into()],
        &bootstrap(),
        &parsed,
        "remote",
    )
    .unwrap();
    assert_eq!(models["new-free"]["api"], "openai-responses");
    assert!(!models.contains_key("jev-1.13-free"));
}
#[test]
fn new_live_free_ids_work_but_paid_retired_and_unverified_zero_cost_names_do_not() {
    let provider = json!({"npm":"@ai-sdk/openai-compatible","models":{"retired-free":{"status":"retired","cost":{"input":0,"output":0}},"zero-cost":{"cost":{"input":0,"output":0}},"paid":{"cost":{"input":1,"output":2}}}});
    let models = build(
        vec![
            "brand-new-free".into(),
            "retired-free".into(),
            "paid".into(),
            "unknown".into(),
            "zero-cost".into(),
        ],
        &provider,
        &BTreeMap::new(),
        "remote",
    )
    .unwrap();
    assert_eq!(models.len(), 2);
    assert!(models.contains_key("brand-new-free"));
    assert!(models.contains_key("zero-cost"));
}
#[test]
fn bootstrap_limits_and_actual_modalities_are_preserved_without_request_metadata() {
    let models = presets();
    let model = &models["muse-spark-1.3-contributor-free"];
    assert_eq!(model["api"], "openai-responses");
    assert_eq!(model["contextWindow"], 1048576);
    assert!(model["input"]
        .as_array()
        .unwrap()
        .iter()
        .any(|value| value == "pdf"));
    assert_eq!(model["info"]["free"], true);
    assert!(model.get("providerOptions").is_none());
}
