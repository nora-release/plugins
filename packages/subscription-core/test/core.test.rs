use super::*;
#[test]
fn routing_usage_never_invents_an_unknown_limit() {
    let value = usage_item("Balance", 2.0, 0.0, None, None, false);
    assert!(value["usedPercent"].is_null());
    assert_eq!(value["countsForRouting"], false);
}
#[test]
fn credentials_cannot_inject_headers() {
    assert!(bearer(&json!({"access":"secret\r\nx-evil: yes"})).is_err());
}
#[test]
fn timestamp_parser_preserves_real_reset_times() {
    assert_eq!(timestamp(&json!(1900000000u64)), Some(1900000000000));
    assert_eq!(
        timestamp(&json!("2030-03-17T17:46:40Z")),
        Some(1900000000000)
    );
    assert_eq!(timestamp(&json!("not a date")), None);
}

#[test]
fn persistent_fingerprint_keeps_previous_sorted_json_bytes() {
    let first: Value = serde_json::from_str(r#"{"z":3,"a":{"y":2,"x":1}}"#).unwrap();
    let second: Value = serde_json::from_str(r#"{"a":{"x":1,"y":2},"z":3}"#).unwrap();
    let expected = "f80735c375180d651e7c5350543887a94bbef5ed349bb2ab7209fcc0758f66e4";
    assert_eq!(fingerprint(&first), fingerprint(&second));
    assert_eq!(fingerprint(&first), expected);
    assert_ne!(fingerprint(&json!([first,2])), fingerprint(&json!([2,second])));
}
