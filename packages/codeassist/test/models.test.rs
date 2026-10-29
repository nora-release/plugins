use super::*;
#[test]
fn all_flash_and_lite_quota_aliases_follow_the_accounts_rollout() {
    for enabled in [false, true] {
        let flags = Flags::parse(
            &json!({"flags":[{"flagId":45842815,"boolValue":enabled},{"flagId":45827489,"boolValue":enabled}]}),
        );
        for alias in ["gemini-3-flash", "gemini-3.5-flash", "gemini-3.8-flash"] {
            assert_eq!(
                flags.shown(alias),
                if enabled {
                    "gemini-3.8-flash"
                } else {
                    "gemini-3.5-flash"
                }
            );
        }
        for alias in ["gemini-3.1-flash-lite", "gemini-3.5-flash-lite"] {
            assert_eq!(
                flags.shown(alias),
                if enabled {
                    "gemini-3.5-flash-lite"
                } else {
                    "gemini-3.1-flash-lite"
                }
            );
        }
    }
}
#[test]
fn experiments_control_both_model_choices_and_wire_names() {
    let flags = Flags::parse(
        &json!({"flags":[{"flagId":45842815,"boolValue":true},{"flagId":45827489,"boolValue":true},{"flagId":45760185,"boolValue":true}]}),
    );
    let models = flags.offered(&json!({"buckets":[{"modelId":"gemini-3-pro-preview"}]}));
    assert!(models.iter().any(|model| model.id == "gemini-3.8-flash"));
    assert!(models
        .iter()
        .any(|model| model.id == "gemini-3.1-pro-preview"));
    assert!(!models.iter().any(|model| model.id == "gemini-3.5-flash"));
    assert_eq!(flags.wire("gemini-3.5-flash"), "gemini-3.8-flash");
    assert_eq!(flags.shown("gemini-3-flash"), "gemini-3.8-flash");
}
#[test]
fn real_catalog_preserves_picker_order_limits_and_collapses_effort_families() {
    let payload: Value =
        serde_json::from_str(include_str!("fixtures/antigravity-models.json")).unwrap();
    let catalog = antigravity(&payload).unwrap();
    assert!(!catalog
        .models
        .iter()
        .any(|model| model.id.starts_with("tab_") || model.id.starts_with("chat_")));
    let flash = catalog
        .models
        .iter()
        .find(|model| model.id == "gemini-3.8-flash")
        .unwrap();
    assert!(flash.info.reasoning_efforts.contains(&"high".into()));
    assert_eq!(flash.context_window, 1048576);
    let wire = catalog.wire("gemini-3.8-flash", Some("high"));
    assert!(payload["models"].get(&wire).is_some());
    assert!(catalog
        .models
        .iter()
        .all(|model| model.api == "google-generative-ai"));
}
#[test]
fn display_name_levels_win_when_they_are_unique_and_single_variants_stay_real_models() {
    let catalog=antigravity(&json!({"models":{"gemini-3.5-flash-extra-low":{"displayName":"Gemini 3.5 Flash (Low)"},"gemini-3.5-flash-low":{"displayName":"Gemini 3.5 Flash (Medium)"},"only-high":{"displayName":"Only (High)"}}})).unwrap();
    assert_eq!(
        catalog.wire("gemini-3.5-flash", Some("low")),
        "gemini-3.5-flash-extra-low"
    );
    assert_eq!(
        catalog.wire("gemini-3.5-flash", Some("medium")),
        "gemini-3.5-flash-low"
    );
    assert!(catalog.models.iter().any(|model| model.id == "only-high"));
}
#[test]
fn empty_and_internal_only_catalogs_fail() {
    assert!(antigravity(&json!({"models":{}})).is_err());
    assert!(antigravity(&json!({"models":{"tab_x":{}}})).is_err());
}
#[test]
fn disabled_models_are_not_offered_until_google_enables_them() {
    let mut payload = json!({"models":{"gemini-3-flash":{},"gemini-4-argon-disabled":{"disabled":true,"displayName":"Gemini 4 Argon","quotaInfo":{"remainingFraction":1}}}});
    let ids = |value: &Value| {
        antigravity(value)
            .unwrap()
            .models
            .into_iter()
            .map(|model| model.id)
            .collect::<Vec<_>>()
    };
    assert_eq!(ids(&payload), vec!["gemini-3-flash"]);
    payload["models"]["gemini-4-argon-disabled"]["disabled"] = json!(false);
    assert!(ids(&payload).contains(&"gemini-4-argon-disabled".into()));
}
