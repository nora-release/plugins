use super::*;
#[test]
fn gemini_quota_is_model_scoped_and_does_not_invent_a_period() {
    let flags = models::Flags::default();
    let result=gemini(&json!({"buckets":[{"modelId":"gemini-3-flash","remainingFraction":0.25,"resetTime":"2030-01-01T00:00:00Z"},{"modelId":"other_vertex","remainingFraction":0},{"modelId":"unknown"}]}),&flags).unwrap();
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["items"][0]["usedPercent"], 75.0);
    assert_eq!(result["items"][0]["modelIDs"], json!(["gemini-3.5-flash"]));
    assert!(result["items"][0].get("windowSeconds").is_none());
}
#[test]
fn antigravity_missing_fraction_in_an_existing_quota_object_means_exhausted_but_absent_quota_is_unknown(
) {
    let result=antigravity(&json!({"models":{"gemini-a":{"quotaInfo":{}},"claude-b":{},"chat_hidden":{"quotaInfo":{}}}}),None).unwrap();
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["items"][0]["usedPercent"], 100.0);
    assert_eq!(result["items"][0]["windowSeconds"], 18000);
    assert_eq!(result["items"][0]["countsForRouting"], true);
}
#[test]
fn shared_summary_windows_remain_display_only_as_in_magpie() {
    let result=antigravity(&json!({"models":{"claude-x":{"quotaInfo":{"remainingFraction":0.9}}}}),Some(&json!({"groups":[{"displayName":"Claude and GPT models","buckets":[{"window":"weekly","remainingFraction":0.4,"resetTime":"2030-01-01T00:00:00Z"},{"window":"5h","disabled":true}]}]}))).unwrap();
    assert_eq!(result["items"].as_array().unwrap().len(), 2);
    assert_eq!(result["items"][1]["windowSeconds"], 604800);
    assert_eq!(result["items"][1]["countsForRouting"], false);
}
#[test]
fn invalid_fractions_are_not_silently_normalized_into_quota() {
    assert!(gemini(
        &json!({"buckets":[{"modelId":"gemini-3-flash","remainingFraction":"NaN"}]}),
        &models::Flags::default()
    )
    .is_err());
    assert!(antigravity(
        &json!({"models":{"gemini-x":{"quotaInfo":{"remainingFraction":2}}}}),
        None
    )
    .is_err());
}
