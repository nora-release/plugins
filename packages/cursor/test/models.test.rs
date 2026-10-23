use super::*;
#[test]
fn remote_picker_preserves_context_sizes_max_mode_and_account_variants() {
    let entries = parse(&crate::fixture::picker(), None).unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(entries[1].id, "claude@1m");
    assert_eq!(entries[1].context, 1_000_000);
    let request = select(&entries, "claude@1m", "high", Some(true));
    assert_eq!(request.id, "claude-high-fast");
    assert!(request.max_mode);
    assert_eq!(request.params["context"], "1m");
    assert_eq!(request.params["fast"], "true");
    let output = output(&entries);
    assert_eq!(output["source"], "remote");
    assert_eq!(output["models"][0]["info"]["temperature"], false);
    assert_eq!(output["models"][0]["info"]["toolCalling"], true);
}
#[test]
fn usable_restrictions_do_not_offer_unavailable_contexts() {
    let entries = parse(
        &crate::fixture::picker(),
        Some(&json!({"models":[{"modelId":"claude-medium"}]})),
    )
    .unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].id, "claude@200k");
    assert_eq!(entries[0].levels.len(), 1);
    assert_eq!(
        select(&entries, "claude@200k", "max", Some(true)).id,
        "claude-medium"
    );
}
#[test]
fn picker_only_families_survive_a_usable_list_that_lags_new_releases() {
    let entries = parse(
        &crate::fixture::picker(),
        Some(&json!({"models":[{"modelId":"not-in-the-picker"}]})),
    )
    .unwrap();
    assert_eq!(entries.len(), 2);
    assert_eq!(tokens("1.5m"), 1_500_000);
    assert_eq!(tokens("unknown"), 0);
    assert!(parse(&json!({"models":[]}), None).is_err());
}
#[test]
fn named_legacy_variants_preserve_their_context_and_fast_flag() {
    let entries = parse(&crate::fixture::picker(), None).unwrap();
    let got = select(&entries, "claude-high-fast", "", None);
    assert!(got.max_mode);
    assert_eq!(got.id, "claude-high-fast");
    assert_eq!(got.params["context"], "1m");
    let auto = select(&entries, "auto", "", None);
    assert_eq!(auto.id, "default");
}

#[test]
fn legacy_families_fit_effort_but_explicit_auto_tier_disables_fast() {
    let entries = parse(&crate::fixture::picker(), None).unwrap();
    let got = select(&entries, "claude-low", "", None);
    assert_eq!(got.id, "claude-medium");
    let got = select(&entries, "claude-high-fast", "", Some(false));
    assert_eq!(got.params["fast"], "false");
    assert_eq!(
        split_id("claude-thinking-extra-high-fast"),
        ("claude-thinking-fast".into(), "xhigh".into(), true)
    );
}
