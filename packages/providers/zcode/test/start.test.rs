use super::*;
use sha2::{Digest, Sha256};
#[test]
fn native_prompt_sections_match_the_upstream_reference_bytes() {
    let value = prompt();
    for (text, expected) in [
        (value["cliPrefix"].as_str().unwrap(), "46dd360a22c87a92"),
        (
            value["stableSections"][0].as_str().unwrap(),
            "3f21ff9a88a03a76",
        ),
        (
            value["stableSections"][1].as_str().unwrap(),
            "39a1e86c93452c01",
        ),
        (
            value["beforeEnvironment"].as_str().unwrap(),
            "bbdc66b399d297ff",
        ),
        (
            value["afterEnvironment"].as_str().unwrap(),
            "1732b7f098a925d7",
        ),
    ] {
        assert_eq!(
            &hex::encode(Sha256::digest(text.as_bytes()))[..16],
            expected
        );
    }
}
#[test]
fn start_dressing_preserves_messages_tools_and_sampling_and_places_cache_only_where_expected() {
    let input = json!({"model":"GLM-5.3-Flash","system":[{"type":"text","text":"My system","cache_control":{"type":"ephemeral"}}],"messages":[{"role":"user","content":[{"type":"text","text":"hello","cache_control":{"type":"ephemeral"}}]},{"role":"assistant","content":[{"type":"tool_use","id":"call","name":"read","input":{}}]},{"role":"user","content":[{"type":"tool_result","tool_use_id":"call","content":"result"}]}],"tools":[{"name":"read","input_schema":{"type":"object"},"cache_control":{"type":"ephemeral"}}],"top_p":0.4,"stop_sequences":["END"]});
    let body = dress(input, Site::Zai, "device", Some("/chosen"), Some(true)).unwrap();
    assert_eq!(body["system"].as_array().unwrap().len(), 4);
    assert!(body["system"][3].get("cache_control").is_none());
    assert!(body["system"][2]["text"]
        .as_str()
        .unwrap()
        .contains("Primary working directory: /chosen"));
    assert_eq!(body["messages"][1]["content"][0]["text"], "hello");
    assert!(body["messages"][1]["content"][0]
        .get("cache_control")
        .is_none());
    assert!(body["messages"][3]["content"][0]
        .get("cache_control")
        .is_some());
    assert!(body["tools"][0].get("cache_control").is_none());
    assert_eq!(body["top_p"], 0.4);
    assert_eq!(body["stop_sequences"], json!(["END"]));
    let metadata: Value =
        serde_json::from_str(body["metadata"]["user_id"].as_str().unwrap()).unwrap();
    assert_eq!(metadata["device_id"], "device");
}
#[test]
fn unknown_working_directory_is_omitted_and_native_headers_do_not_leak_the_device_key() {
    let body = dress(
        json!({"model":"GLM-5.3-Flash","messages":[]}),
        Site::BigModel,
        "device",
        None,
        None,
    )
    .unwrap();
    assert!(!body["system"][2]["text"]
        .as_str()
        .unwrap()
        .contains("Primary working directory:"));
    let headers = headers("jwt-with-no-expiry").unwrap();
    assert_eq!(headers["authorization"], "Bearer jwt-with-no-expiry");
    assert!(!headers.contains_key("x-device-mid"));
    assert!(!headers.contains_key("x-api-key"));
    assert_eq!(headers["x-zcode-agent"], "glm");
}
#[test]
fn spent_bucket_codes_and_network_blocks_remain_distinct() {
    assert!(spent(&json!({"error":{"code":"1113"}}), 200));
    assert!(spent(&json!({"code":1005}), 200));
    assert!(!spent(&json!({"code":3012}), 405));
    assert!(refused(405, b"blocked"));
    assert!(refused(403, br#"{"code":3012}"#));
    assert!(!refused(429, br#"{"code":1005}"#));
}
