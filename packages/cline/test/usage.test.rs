use super::*;
#[test]
fn dollar_millionths_are_not_misreported_as_credits_or_a_percentage() {
    let value = parse(
        &json!({"planName":"ClinePass"}),
        &json!({"balance":5000}),
        &json!({"limits":[{"type":"five_hour","percentUsed":25,"resetsAt":"2030-03-17T17:46:40Z"},{"type":"unknown","percentUsed":100}]}),
        "host-account",
    );
    assert_eq!(value["balance"], "$0.0050");
    assert_eq!(value["items"].as_array().unwrap().len(), 1);
    assert_eq!(value["items"][0]["usedPercent"], 25.0);
    assert_eq!(value["items"][0]["countsForRouting"], true);
}
#[test]
fn absolute_balance_never_looks_like_one_hundred_percent_remaining() {
    let value = parse(
        &Value::Null,
        &json!({"remainingCredits":7.5}),
        &Value::Null,
        "a",
    );
    assert_eq!(value["balance"], "7.5 credits");
    assert!(value["items"].as_array().unwrap().is_empty());
}
#[test]
fn total_and_used_credits_form_a_real_routing_window() {
    let value = parse(
        &Value::Null,
        &json!({"credits":{"total":100,"used":30}}),
        &Value::Null,
        "a",
    );
    assert_eq!(value["items"][0]["usedPercent"], 30.0);
    assert_eq!(value["items"][0]["used"], 30.0);
}
