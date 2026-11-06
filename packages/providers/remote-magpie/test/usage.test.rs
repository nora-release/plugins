use super::*;
use crate::test_support::{Client, Directory};
fn account() -> Account {
    Account::inputs(
        &json!({"endpoint":"http://remote.test:3572","apiKey":"private","name":"Office"}),
    )
    .unwrap()
}
fn reading(value: Value) -> Reading {
    let mut reading = Reading::default();
    reading.value = Some(value);
    reading.read_at = 1_800_000_000_000;
    reading
}
fn key() -> Reading {
    reading(
        json!({"limited":true,"key":"Laptop","limit":{"period":"day","tokens":40,"tokenLimit":100,"tokensLeft":60,"spent":false,"reset":"2026-10-23T00:00:00Z"}}),
    )
}
#[test]
fn remote_cards_are_namespaced_and_read_only_without_becoming_local_accounts() {
    let cards = reading(
        json!({"data":[{"provider":"codex","name":"Codex","user":"upstream@example.com","kind":"subscription","windows":[{"name":"5h","used":100,"resetsAt":"2026-10-23T00:00:00Z"}],"resets":{"count":2},"checkins":true,"checkin":{"ok":true}},{"provider":"deepseek","name":"DeepSeek","kind":"balance","balance":"¥12.00"},{"provider":"third/codex","name":"Other","from":"Third","windows":[]}]}),
    );
    let result = render(&account(), "local-key-one", &cards, &key());
    let cards = result["remoteCards"].as_array().unwrap();
    assert_eq!(cards.len(), 2);
    assert_eq!(cards[0]["provider"], "remote-magpie/local-key-one/codex");
    assert_eq!(cards[0]["user"], "upstream@example.com");
    assert_eq!(cards[0]["name"], "Codex · Office");
    assert!(cards[0].get("resets").is_none());
    assert!(cards[0].get("checkin").is_none());
    assert!(result.get("email").is_none());
    let windows = result["items"].as_array().unwrap();
    assert!(windows
        .iter()
        .filter(|item| item["source"] == "remote")
        .all(|item| item["countsForRouting"] == false));
    assert_eq!(result["routing"]["allowed"], true);
    assert!(windows.iter().any(|item| item["value"] == "¥12.00"));
}
#[test]
fn each_remote_key_limit_keeps_its_real_period_units_and_reset() {
    let status = json!({"limited":true,"key":"Laptop","limit":{"period":"day","tokens":100,"tokenLimit":100,"spent":true,"reset":"2026-10-23T00:00:00Z"},"limits":[{"period":"day","tokens":100,"tokenLimit":100,"spent":true,"reset":"2026-10-23T00:00:00Z"},{"period":"month","cost":20.5,"costLimit":20,"spent":true,"reset":"2026-11-01T00:00:00Z"}]});
    let result = render(&account(), "key", &Reading::default(), &reading(status));
    assert_eq!(result["routing"]["allowed"], false);
    assert_eq!(result["routing"]["resetsAt"], "2026-11-01T00:00:00Z");
    let items = result["items"].as_array().unwrap();
    assert!(items.iter().any(|item| item["unit"] == "tokens"
        && item["used"].as_f64() == Some(100.0)
        && item["countsForRouting"] == true));
    assert!(items
        .iter()
        .any(|item| item["unit"] == "USD" && item["used"] == 20.5 && item["usedPercent"] == 100.0));
}
#[test]
fn restricted_keys_never_show_cached_remote_accounts() {
    let mut cards = reading(json!({"data":[{"provider":"private","name":"Private","windows":[]}]}));
    cards.held = true;
    let result = render(&account(), "key", &cards, &key());
    assert_eq!(result["remoteCards"], json!([]));
    assert_eq!(result["remoteAccountsRestricted"], true);
    assert!(!result.to_string().contains("Private"));
    assert_eq!(result["gatewayKey"]["name"], "Laptop");
}
#[test]
fn no_reading_is_not_an_unlimited_key_and_an_unknown_reset_is_not_invented() {
    let result = render(&account(), "key", &Reading::default(), &Reading::default());
    assert_eq!(result["available"], false);
    assert!(result["gatewayKey"].is_null());
    let result = render(
        &account(),
        "key",
        &Reading::default(),
        &reading(json!({"limited":true,"limit":{"tokens":10,"tokenLimit":10,"spent":true}})),
    );
    assert_eq!(result["routing"]["allowed"], false);
    assert!(result["routing"].get("resetsAt").is_none());
}
#[tokio::test]
async fn only_the_remote_cache_and_the_current_keys_limits_are_read_and_cached() {
    let directory = Directory::new();
    let client = Client::default();
    client.push(
        200,
        json!({"data":[{"provider":"codex","name":"Codex","windows":[{"name":"5h","used":5}]}]}),
    );
    client.push(200, json!({"limited":false,"key":"Laptop"}));
    let first = read(&directory.0, &client, &account(), "key", false)
        .await
        .unwrap();
    assert_eq!(first["gatewayKey"]["limited"], false);
    let second = read(&directory.0, &client, &account(), "key", false)
        .await
        .unwrap();
    assert_eq!(first, second);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests
        .iter()
        .all(|request| request.headers["authorization"] == "Bearer private"));
    assert!(requests.iter().any(|request| request.url.ends_with(CARDS)));
    assert!(requests.iter().any(|request| request.url.ends_with(LIMIT)));
}
#[tokio::test]
async fn an_explicit_remote_card_refresh_preserves_provider_and_user_then_rereads() {
    let directory = Directory::new();
    let client = Client::default();
    client.push(200, json!({"data":[]}));
    client.push(200, json!({"data":[]}));
    client.push(200, json!({"limited":false}));
    refresh(
        &directory.0,
        &client,
        &account(),
        "key",
        &json!({"provider":"codex","user":"a+x@example.com"}),
    )
    .await
    .unwrap();
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests[0].method, HttpMethod::Post);
    assert!(requests[0]
        .url
        .ends_with("/v1/magpie/quotas/refresh?provider=codex&user=a%2Bx%40example.com"));
    assert_eq!(requests.len(), 3);
}
#[tokio::test]
async fn a_failed_refresh_keeps_the_last_key_numbers_with_an_error_instead_of_zeroing_them() {
    let directory = Directory::new();
    let client = Client::default();
    client.push(200, json!({"data":[]}));
    client.push(200, key().value.unwrap());
    read(&directory.0, &client, &account(), "key", false)
        .await
        .unwrap();
    client.push(502, json!({"error":{"message":"offline"}}));
    client.push(502, json!({"error":{"message":"offline"}}));
    let stale = read(&directory.0, &client, &account(), "key", true)
        .await
        .unwrap();
    assert_eq!(stale["gatewayKey"]["limit"]["tokens"], 40);
    assert_eq!(stale["stale"], true);
    assert!(stale["error"].as_str().unwrap().contains("offline"));
}
