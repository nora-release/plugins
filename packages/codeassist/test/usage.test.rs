use super::*;
#[test]
fn exhausted_lite_quota_is_scoped_to_the_rolled_out_model() {
    let flags = models::Flags::parse(&json!({"flags":[{"flagId":45827489,"boolValue":true}]}));
    let result = gemini(
        &json!({"buckets":[{"modelId":"gemini-3.1-flash-lite","remainingFraction":0}]}),
        &flags,
    )
    .unwrap();
    assert_eq!(result["items"][0]["usedPercent"], 100.0);
    assert_eq!(
        result["items"][0]["modelIDs"],
        json!(["gemini-3.5-flash-lite"])
    );
}

#[test]
fn antigravity_versioned_weekly_pool_wins_over_family_pool_in_either_order() {
    let models = json!({"models":{"gemini-3-flash":{"quotaInfo":{"remainingFraction":0.9}},"gemini-4-argon-high":{"displayName":"Gemini 4 Argon (High)","quotaInfo":{"remainingFraction":0.5}},"claude-sonnet-4-6":{"quotaInfo":{"remainingFraction":0.8}},"gpt-oss-120b":{"quotaInfo":{"remainingFraction":0.7}}}});
    let family = json!({"displayName":"Gemini Models","buckets":[{"window":"5h","remainingFraction":0.9},{"window":"weekly","remainingFraction":0.8}]});
    let specific = json!({"displayName":"Gemini 4 Argon","buckets":[{"window":"WEEKLY","remainingFraction":0.5}]});
    let claude = json!({"displayName":"Claude and GPT models","description":"Claude and GPT-OSS","buckets":[{"window":"5h","remainingFraction":0.7}]});
    for groups in [
        json!([family.clone(), specific.clone(), claude.clone()]),
        json!([specific.clone(), family.clone(), claude.clone()]),
    ] {
        let result = antigravity(&models, Some(&json!({"groups":groups}))).unwrap();
        let items = result["items"].as_array().unwrap();
        let get = |id: &str| items.iter().find(|item| item["id"] == id).unwrap();
        assert_eq!(get("gemini-3-flash")["pool"], "Gemini");
        assert_eq!(get("gemini-4-argon-high")["pool"], "Gemini 4 Argon");
        assert_eq!(get("gemini-4-argon-high")["family"], "Gemini");
        assert_eq!(get("claude-sonnet-4-6")["pool"], "Claude & GPT");
        assert_eq!(get("gpt-oss-120b")["family"], "GPT-OSS");
        assert_eq!(get("gpt-oss-120b")["pool"], "Claude & GPT");
        assert!(items
            .iter()
            .filter(|item| item.get("family").is_none())
            .all(|item| item["countsForRouting"] == false));
    }
}

#[test]
fn broad_weekly_only_pool_keeps_per_model_quota_visible() {
    let result=antigravity(&json!({"models":{"gemini-3-flash":{"quotaInfo":{"remainingFraction":0.5}}}}),Some(&json!({"groups":[{"displayName":"Gemini Models","buckets":[{"window":"weekly","remainingFraction":0.5}]}]}))).unwrap();
    let model = result["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|item| item["id"] == "gemini-3-flash")
        .unwrap();
    assert_eq!(model["family"], "Gemini");
    assert!(model.get("pool").is_none());
}
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
#[test]
fn disabled_model_allowances_do_not_make_an_unusable_account_look_available() {
    let result=antigravity(&json!({"models":{"gemini-3-flash":{"quotaInfo":{"remainingFraction":0.5}},"gemini-4-argon-disabled":{"disabled":true,"quotaInfo":{"remainingFraction":1}}}}),None).unwrap();
    assert_eq!(result["items"].as_array().unwrap().len(), 1);
    assert_eq!(result["items"][0]["id"], "gemini-3-flash");
}
#[test]
fn effort_variants_share_a_family_allowance_even_when_the_caller_names_a_raw_variant() {
    let result=antigravity(&json!({"models":{"gemini-3.7-flash-high":{"quotaInfo":{"remainingFraction":0}},"gemini-3.7-flash-low":{"quotaInfo":{"remainingFraction":1}}}}),None).unwrap();
    let items = result["items"].as_array().unwrap();
    let exhausted = items
        .iter()
        .find(|item| item["id"] == "gemini-3.7-flash-high")
        .unwrap();
    for model in [
        "gemini-3.7-flash",
        "gemini-3.7-flash-high",
        "gemini-3.7-flash-low",
    ] {
        assert!(exhausted["modelIDs"]
            .as_array()
            .unwrap()
            .iter()
            .any(|id| id == model));
    }
}
