use super::*;
#[test]
fn only_named_feed_groups_add_models_and_free_membership_wins_duplicates() {
    let rows=parse(&json!({"recommended":["maker/model",{"id":"other/model","display_name":"Other"}],"free":["maker/model"],"clinePass":["cline-pass/real"],"description":"fake/not-model","clineCloud":["hidden/model"]}),&Value::Null,"remote").unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["info"]["free"], true);
    assert!(rows[0]["name"].as_str().unwrap().ends_with("(free)"));
    assert!(rows[2]["name"].as_str().unwrap().ends_with("(ClinePass)"));
}
#[test]
fn cloud_catalog_adds_actual_windows_and_only_unanimous_fallback_modalities() {
    let rows=parse(&json!({"recommended":["cline-pass/shared","maker/own"]}),&json!({"data":[{"id":"maker/shared","architecture":{"input_modalities":["text","image"]}},{"id":"maker/own","context_length":123456,"architecture":{"input_modalities":["text","image"]}},{"id":"other/shared","architecture":{"input_modalities":["text"]}}]}),"remote").unwrap();
    assert_eq!(rows[0]["input"], json!(["text"]));
    assert_eq!(rows[1]["contextWindow"], 123456);
    assert_eq!(rows[1]["input"], json!(["text", "image"]));
    assert!(rows.iter().all(|row| row["maxTokens"] == 0));
}
