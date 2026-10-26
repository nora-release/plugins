use super::*;
use serde_json::json;

fn definition() -> Definition {
    Definition::parse(json!({
        "id":"test","name":"Test","api":"openai-completions","baseURL":"https://example.invalid/v1",
        "config":{"keyProvider":"openai","oauthProvider":"openai","passUserAgent":true}
    }))
    .unwrap()
}

#[test]
fn only_explicit_api_connections_accept_the_callers_identity() {
    let mut definition = definition();
    let api = json!({"type":"api","key":"fixture-key"});
    let mut headers = BTreeMap::from([("User-Agent".into(), "Nora".into())]);
    apply(
        &definition,
        &api,
        Some("Caller/2.3 (Mac OS X)"),
        &mut headers,
    );
    assert_eq!(headers["User-Agent"], "Caller/2.3 (Mac OS X)");
    for (enabled, owned, credentials, caller) in [
        (false, false, api.clone(), Some("Caller/2.3")),
        (true, true, api.clone(), Some("Caller/2.3")),
        (
            true,
            false,
            json!({"type":"oauth","access":"fixture"}),
            Some("Caller/2.3"),
        ),
        (true, false, api.clone(), None),
        (true, false, api.clone(), Some("nora-router/1")),
        (true, false, api.clone(), Some("Nora/1")),
        (true, false, api.clone(), Some("invalid\nheader")),
    ] {
        definition.config.pass_user_agent = enabled;
        definition.config.own_client = owned;
        let mut headers = BTreeMap::from([("User-Agent".into(), "Nora".into())]);
        apply(&definition, &credentials, caller, &mut headers);
        assert_eq!(headers["User-Agent"], "Nora");
    }
}

#[test]
fn explicit_headers_win_case_insensitively_without_duplicate_user_agents() {
    let mut definition = definition();
    definition
        .config
        .headers
        .insert("uSeR-aGeNt".into(), "Custom/1".into());
    let mut headers = BTreeMap::from([("user-agent".into(), "Old/1".into())]);
    apply(
        &definition,
        &json!({"type":"api","key":"fixture"}),
        Some("Caller/2"),
        &mut headers,
    );
    headers.extend(definition.config.headers);
    assert_eq!(headers.len(), 1);
    assert_eq!(headers["uSeR-aGeNt"], "Custom/1");
}
