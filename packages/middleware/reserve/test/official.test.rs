use nora_gateway_middleware::Middleware;
use serde_json::{json, Value};
#[test]
fn official_cases() {
    let cases: Value = serde_json::from_str(include_str!("fixtures/strategies.json")).unwrap();
    for c in cases.as_array().unwrap() {
        let seats = c["seats"].as_array().unwrap();
        let order = nora_provider_reserve::Plugin
            .order(
                c["strategy"].as_str().unwrap(),
                c["pool"].as_str().unwrap_or("test-pool"),
                seats,
                c.get("options").unwrap_or(&json!({})),
            )
            .unwrap();
        let keys = order
            .iter()
            .map(|&i| seats[i].get("key").cloned().unwrap_or(json!(i)))
            .collect::<Vec<_>>();
        assert_eq!(json!(keys), c["expect"], "{}", c["name"]);
    }
}
#[test]
fn declaration_contains_only_the_two_strategy_exports() {
    let manifest: Value = serde_json::from_str(include_str!("../nora-plugin.json")).unwrap();
    let declaration = &manifest["contributes"]["middleware"];
    assert_eq!(
        declaration["strategies"],
        json!(["ceiling", "accounts-first"])
    );
    for phase in ["request", "provider", "event", "response"] {
        assert!(declaration.get(phase).is_none());
    }
}
#[test]
fn every_strategy_always_returns_a_permutation_for_all_official_seat_edges() {
    for name in ["ceiling", "accounts-first"] {
        for n in 2..9 {
            let seats=(0..n).map(|i|json!({"account":i%2==0,"known":i%3!=0,"used":(i*37)%101,"resting":i==4,"weight":i%3+1,"tokens":i*1000})).collect::<Vec<_>>();
            let mut order = nora_provider_reserve::Plugin
                .order(name, "group/fast", &seats, &json!({"ceiling":50}))
                .unwrap();
            order.sort();
            assert_eq!(order, (0..n).collect::<Vec<_>>());
        }
    }
}
