use super::*;
#[test]
fn media_signatures_keep_previous_sorted_bytes() {
    let a: Value =
        serde_json::from_str(r#"[{"type":"image_url","image_url":{"url":"fixture"}}]"#).unwrap();
    let b: Value =
        serde_json::from_str(r#"[{"image_url":{"url":"fixture"},"type":"image_url"}]"#).unwrap();
    assert_eq!(signature(&a), signature(&b));
    assert_eq!(
        signature(&a),
        "[image_url:9a118d04ec4e2d1c8639944714ec05c8d035a3cc4fcbd228d5ec167449b787be]"
    );
}
