use super::*;

#[test]
fn included_credits_count_for_routing_while_on_demand_spending_does_not() {
    let usage = parse(&json!({"config": {
        "creditUsagePercent": 25,
        "currentPeriod": {"type": "WEEKLY", "end": "2026-11-01T00:00:00Z"},
        "onDemandUsed": {"val": 100},
        "onDemandCap": {"val": 100}
    }}));
    let items = usage["items"].as_array().unwrap();
    assert_eq!(items.len(), 2);
    assert_eq!(items[0]["id"], "included_credits");
    assert_eq!(items[0]["countsForRouting"], true);
    assert_eq!(items[0]["usedPercent"], 25.0);
    assert!(timestamp(&items[0]["resetsAt"]).is_some());
    assert_eq!(items[1]["id"], "on_demand");
    assert_eq!(items[1]["countsForRouting"], false);
    assert_eq!(items[1]["usedPercent"], 100.0);
}
