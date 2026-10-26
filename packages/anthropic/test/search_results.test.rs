use super::*;
fn result() -> Value {
    serde_json::from_str(include_str!("testdata/cli-web-search-result.json")).unwrap()
}
#[test]
fn only_a_known_cli_search_result_can_supply_public_sources() {
    let value = result();
    let mut history = History::default();
    assert!(history.result(&value).unwrap().is_none());
    history.call("search-own").unwrap();
    let source = history.result(&value).unwrap().unwrap();
    assert!(source.id.starts_with("srvtoolu_"));
    assert_eq!(source.query, "official documentation");
    assert_eq!(
        source.pages,
        json!([{"type":"web_search_result","title":"Example documentation","url":"https://example.com/docs","page_age":"today","encrypted_content":""}])
    );
    assert!(history.result(&value).unwrap().is_none());
    history.call("search-own").unwrap();
    assert!(history.result(&value).unwrap().is_none());
}
#[test]
fn private_fields_and_credential_or_non_web_urls_never_become_sources() {
    let mut value = result();
    value["tool_use_result"]["accessToken"] = json!("private-token");
    value["tool_use_result"]["results"][1]["content"][0]["encrypted_content"] =
        json!("private-encrypted-blob");
    value["tool_use_result"]["results"][1]["content"][0]["credentials"] =
        json!({"refresh":"private-refresh"});
    value["tool_use_result"]["results"]
        .as_array_mut()
        .unwrap()
        .push(json!({"content":[
            {"url":"file:///private/local","title":"private file"},
            {"url":"https://user:password@example.com/docs","title":"private auth"},
            {"url":"https://example.com/docs?access_token=private-token","title":"private query"},
            {"url":"data:text/plain,private-blob","title":"private blob"}
        ]}));
    let mut history = History::default();
    history.call("search-own").unwrap();
    let source = history.result(&value).unwrap().unwrap();
    assert_eq!(source.pages.as_array().unwrap().len(), 1);
    let encoded = json!(source.pages).to_string();
    for private in [
        "private-token",
        "private-encrypted-blob",
        "private-refresh",
        "private file",
        "private auth",
        "private query",
        "private blob",
    ] {
        assert!(!encoded.contains(private));
    }
}
#[test]
fn ordinary_mcp_results_are_not_reclassified_as_native_search() {
    let mut value = result();
    value["message"]["content"][0]["tool_use_id"] = json!("mcp-call");
    let mut history = History::default();
    history.call("search-own").unwrap();
    assert!(history.result(&value).unwrap().is_none());
    assert_eq!(
        value["message"]["content"][0]["content"],
        "Web search results for query: official documentation"
    );
    value["message"]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"tool_result","tool_use_id":"search-own","content":"public search"}));
    assert!(history.result(&value).unwrap().is_some());
}

#[test]
fn failed_or_malformed_cli_tool_results_cannot_invent_sources() {
    let mut value = result();
    let mut history = History::default();
    history.call("search-own").unwrap();
    value["message"]["content"][0]["is_error"] = json!(true);
    assert!(history.result(&value).unwrap().is_none());
    value["message"]["content"][0]["is_error"] = json!(false);
    value["tool_use_result"]["results"] = json!({"private":"must-not-copy"});
    assert!(history.result(&value).unwrap().is_none());
    value["tool_use_result"]["results"] = json!(["No result pages were returned."]);
    let source = history.result(&value).unwrap().unwrap();
    assert_eq!(source.pages, json!([]));
}
