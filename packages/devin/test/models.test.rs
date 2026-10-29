use super::*;
use crate::fixture;
#[test]
fn real_upstream_protobuf_fixtures_define_live_families_images_and_prices() {
    for bytes in [
        include_bytes!("model-configs.bin").as_slice(),
        include_bytes!("model-configs-1015.bin").as_slice(),
    ] {
        let rows = parse(bytes).unwrap();
        assert!(!rows.is_empty());
        let listing = output(&rows);
        assert_eq!(listing["source"], "remote");
        assert!(!listing["models"].as_array().unwrap().is_empty());
        assert!(rows.iter().any(|r| r.image));
        assert!(rows.iter().any(|r| r.free));
    }
}
#[test]
fn variant_selection_uses_real_family_effort_and_fast_tier() {
    let rows = parse(&fixture::configs()).unwrap();
    assert_eq!(variant(&rows, "swe-2", "medium").unwrap(), "swe-2-medium");
    assert_eq!(
        variant(&rows, "claude-opus-5-5", "medium").unwrap(),
        "claude-opus-5-5-high"
    );
    assert_eq!(
        variant(&rows, "claude-opus-5-5-fast", "low").unwrap(),
        "claude-opus-5-5-high-fast"
    );
    assert_eq!(
        variant(&rows, "claude-opus-5-5-low", "max").unwrap(),
        "claude-opus-5-5-low"
    );
    assert!(variant(&rows, "nonexistent", "high").is_err());
}
#[test]
fn free_family_and_all_free_variants_are_excluded_from_quota_scope() {
    let rows = parse(&fixture::configs()).unwrap();
    let ids = paid_ids(&rows);
    assert!(!ids.iter().any(|id| id.starts_with("swe-2")));
    assert!(ids.contains(&"claude-opus-5-5".into()));
    assert!(ids.contains(&"claude-opus-5-5-high".into()));
    let listing = output(&rows);
    let swe = listing["models"]
        .as_array()
        .unwrap()
        .iter()
        .find(|m| m["id"] == "swe-2")
        .unwrap();
    assert_eq!(swe["info"]["free"], true);
    assert_eq!(swe["info"]["pricingKnown"], true);
}
#[test]
fn incomplete_or_duplicate_registries_do_not_become_fake_model_lists() {
    assert!(parse(&[]).is_err());
    let row = Pb::new().text(22, "same");
    assert!(parse(&Pb::new().bytes(1, &row).bytes(1, &row).0).is_err());
    let mut bytes = fixture::configs();
    bytes.push(128);
    assert!(parse(&bytes).is_err());
}

#[test]
fn actual_october_prices_keep_free_swe_family_separate_from_paid_lightning() {
    let rows = parse(include_bytes!("model-configs-1015.bin")).unwrap();
    let listing = output(&rows);
    let models = listing["models"].as_array().unwrap();
    let swe = models.iter().find(|m| m["id"] == "swe-2").unwrap();
    assert_eq!(swe["info"]["free"], true);
    let glm = rows.iter().find(|m| m.id == "glm-5-2").unwrap();
    assert_eq!(glm.cost.as_ref().unwrap()["input"], 1.4);
    assert_eq!(glm.cost.as_ref().unwrap()["cacheRead"], 0.26);
    let paid = paid_ids(&rows);
    assert!(!paid.contains(&"swe-2".into()));
    assert!(paid.contains(&"swe-2-high-lightning".into()));
}
