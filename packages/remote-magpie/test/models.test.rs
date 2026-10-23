use super::*;
fn account() -> Account {
    Account::inputs(&json!({"endpoint":"https://remote.test/root","apiKey":"key"})).unwrap()
}
#[test]
fn real_magpie_labels_ids_context_and_reasoning_are_preserved() {
    let payload: Value = serde_json::from_str(include_str!("fixtures/magpie-models.json")).unwrap();
    let list = parse(&payload, &account()).unwrap();
    let entries = list["models"].as_array().unwrap();
    assert_eq!(entries.len(), 3);
    assert_eq!(entries[1]["id"], "relaya/deepseek-chat");
    assert_eq!(entries[1]["name"], "DeepSeek Chat · Relay A");
    assert_eq!(entries[2]["contextWindow"], 202752);
    assert_eq!(entries[2]["maxTokens"], 131072);
    assert_eq!(entries[2]["reasoning"], true);
    assert_eq!(list["allowUnknownModels"], false);
}
#[test]
fn native_protocols_choose_real_endpoints_without_an_openai_detour() {
    let list=parse(&json!({"data":[{"id":"claude","native_endpoints":["/v1/messages"]},{"id":"codex","native_endpoints":["/v1/responses"],"service_tiers":[{"id":"priority"}],"supported_reasoning_levels":[{"effort":"high"}]},{"id":"old","display_name":"Old"},{"id":"nora","native_endpoints":["anthropic"]}]}),&account()).unwrap();
    let list = &list["models"];
    assert_eq!(list[0]["api"], "anthropic-messages");
    assert_eq!(list[0]["baseURL"], "https://remote.test/root");
    assert_eq!(list[1]["api"], "openai-responses");
    assert_eq!(list[1]["baseURL"], "https://remote.test/root/v1");
    assert_eq!(list[1]["info"]["serviceTiers"], json!(["priority"]));
    assert_eq!(list[1]["info"]["reasoningEfforts"], json!(["high"]));
    assert_eq!(list[2]["api"], "openai-completions");
    assert_eq!(list[3]["api"], "anthropic-messages");
}
#[test]
fn arbitrary_media_retrieval_and_decision_names_do_not_become_chat_models() {
    let list=parse(&json!({"data":[{"id":"plain-image","kind":"image"},{"id":"grok-imagine-video","kind":"video"},{"id":"custom-judge","kind":"decision"},{"id":"library/embed","kind":"embedding"},{"id":"library/rank","kind":"rerank"}]}),&account()).unwrap();
    let entries = list["models"].as_array().unwrap();
    assert!(entries.iter().all(|model| model["info"]["chat"] == false));
    assert_eq!(entries[0]["info"]["imageOutput"], true);
    assert_eq!(entries[1]["info"]["imageOutput"], false);
    assert_eq!(entries[1]["info"]["videoOutput"], true);
    assert_eq!(entries[2]["api"], "systemone");
    assert_eq!(entries[3]["info"]["embedding"], true);
    assert_eq!(entries[4]["info"]["nativeKind"], "rerank");
}
#[test]
fn explicit_remote_capabilities_override_model_name_guesses() {
    let list=parse(&json!({"data":[{"id":"image-sounding-chat","capabilities":{"chat":true,"tools":false,"vision":true,"reasoning":true}},{"id":"private-embed","capabilities":{"chat":false,"embedding":true}}]}),&account()).unwrap();
    assert_eq!(list["models"][0]["info"]["chat"], true);
    assert_eq!(list["models"][0]["info"]["toolCalling"], false);
    assert_eq!(list["models"][0]["input"], json!(["text", "image"]));
    assert_eq!(list["models"][1]["info"]["chat"], false);
}
#[test]
fn bad_prices_and_bad_entries_do_not_lose_the_whole_remote_list() {
    let list=parse(&json!({"data":[{"id":"good","magpie_price":{"input":2,"output":5}},{"id":"bad-price","magpie_price":"cheap"},{"id":"good","name":"duplicate"},{"id":"bad\n"},{}]}),&account()).unwrap();
    assert_eq!(list["models"].as_array().unwrap().len(), 2);
    assert_eq!(list["models"][0]["cost"]["input"], 2.0);
    assert!(list["models"][1].get("cost").is_none());
    assert!(parse(&json!({"data":{}}), &account()).is_err());
}
