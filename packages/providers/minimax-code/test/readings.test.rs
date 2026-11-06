use super::*;
#[test]
fn account_model_order_and_actual_thinking_modes_are_kept() {
    let site = Site::for_id("minimax-code").unwrap();
    let value = json!({"providers":[{"providerId":"other","config":{"models":{"fake":{}}}},{"providerId":"minimax","config":{"model_order":["new","switch","new"],"models":{"new":{"name":"Real new model","limit":{"context":1000000,"output":1000},"thinking_config":{"mode":"forced_on"},"thinking":{"effortOptions":["default","low","high"]}},"switch":{"modalities":{"input":["text","image"]},"thinking_config":{"mode":"switchable"}}}}}]});
    let rows = models(&value, site, "remote").unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0]["id"], "new");
    assert_eq!(rows[0]["info"]["reasoningEfforts"], json!(["low", "high"]));
    assert_eq!(rows[1]["input"], json!(["text", "image"]));
    assert_eq!(rows[1]["info"]["reasoningEfforts"], json!(["off", "high"]));
}
#[test]
fn only_general_plan_windows_change_account_routing() {
    let rows=plan_windows(&json!({"base_resp":{"status_code":0},"model_remains":[{"model_name":"general","current_interval_remaining_percent":75,"current_interval_status":1,"start_time":1900000000u64,"end_time":1900018000u64,"current_weekly_remaining_percent":40,"current_weekly_status":1,"weekly_end_time":1900604800u64},{"model_name":"image","current_interval_remaining_percent":0,"current_interval_status":2}]})).unwrap();
    assert_eq!(rows.len(), 3);
    assert_eq!(rows[0]["usedPercent"], 25.0);
    assert_eq!(rows[0]["windowSeconds"], 18000.0);
    assert_eq!(rows[0]["countsForRouting"], true);
    assert_eq!(rows[2]["usedPercent"], 100.0);
    assert_eq!(rows[2]["countsForRouting"], false);
}
#[test]
fn unavailable_windows_are_not_fabricated() {
    assert!(plan_windows(&json!({"error":"unavailable"})).is_err());
    assert!(plan_windows(&json!({"model_remains":[{"model_name":"general","current_interval_status":3,"current_weekly_status":3}]})).unwrap().is_empty());
}
#[test]
fn cn_and_global_presets_are_explicitly_presets_with_separate_origins() {
    for id in ["minimax-code", "minimax-code-global"] {
        let rows = presets(Site::for_id(id).unwrap());
        assert_eq!(rows.len(), 4);
        assert!(rows
            .iter()
            .all(|row| row["provider"] == id && row["info"]["catalogSource"] == "preset"));
    }
}
