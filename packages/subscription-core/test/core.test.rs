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
