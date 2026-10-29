use super::*;
use serde_json::json;
use std::collections::BTreeMap;
#[test]
fn only_plugin_declared_client_metadata_reaches_an_upstream_and_account_state_never_does() {
    let mut definition=Definition::from_fields(json!({"id":"forward","name":"Forward","api":"openai-completions","baseURL":"https://example.invalid/v1","config":{"keyProvider":"forward","forwardedHeaders":["x-openai-*","x-codex-*","session_id"]}})).unwrap();
    let mut request:FetchRequest=serde_json::from_value(json!({"url":"https://example.invalid/v1/responses","method":"POST","metadata":{"header:x-openai-subagent":"guardian","header:x-codex-installation-id":"installation","header:x-codex-turn-state":"private-account-state","header:x-openai-codex-luna-reserve":"1","header:x-openai-actor-authorization":"private-token","header:session_id":"session","header:unrelated":"no"}})).unwrap();
    apply(&definition, &mut request).unwrap();
    assert_eq!(request.headers.len(), 3);
    assert_eq!(request.headers["x-openai-subagent"], "guardian");
    assert!(!request
        .headers
        .values()
        .any(|value| value.contains("private")));
    definition.config.forwarded_headers.clear();
    request.headers = BTreeMap::new();
    apply(&definition, &mut request).unwrap();
    assert!(request.headers.is_empty());
}
