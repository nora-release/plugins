use super::*;
use crate::{fixture, protobuf::fields};
#[test]
fn unsafe_tool_ids_round_trip_exactly_without_collisions() {
    for id in ["Bash:0#a65b6", "dv_existing", "工具:abc", "plain-safe_1"] {
        let safe = out_id(id);
        assert!(safe
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"_-".contains(&b)));
        assert_eq!(in_id(&safe), id);
    }
    assert_ne!(out_id("dv_existing"), "dv_existing");
    assert_eq!(in_id("dv_not-canonical"), "dv_not-canonical");
}
#[test]
fn split_parallel_calls_keep_original_calls_and_both_real_results() {
    let chat = json!({"messages":[{"role":"user","content":"hi"},{"role":"assistant","content":"look","tool_calls":[{"id":"a","function":{"name":"read","arguments":"{\"path\":\"a\"}"}}]},{"role":"assistant","tool_calls":[{"id":"b","function":{"name":"bash","arguments":"{}"}}]},{"role":"tool","tool_call_id":"a","content":"A"},{"role":"tool","tool_call_id":"b","content":"B"}]});
    let (msgs, _) = messages(&chat).unwrap();
    assert_eq!(
        msgs.iter().map(|m| m.role).collect::<Vec<_>>(),
        vec![1, 2, 4, 4]
    );
    assert_eq!(msgs[1].calls.len(), 2);
    assert_eq!(msgs[2].answer, "a");
    assert_eq!(msgs[3].answer, "b");
    assert_eq!(msgs[2].text, "A");
    assert_eq!(msgs[3].text, "B");
}
#[test]
fn missing_results_are_explicit_and_duplicate_or_unowned_results_refused() {
    let call = json!({"role":"assistant","tool_calls":[{"id":"a","function":{"name":"read","arguments":"{}"}}]});
    let (chat, _) = messages(&json!({"messages":[call.clone()]})).unwrap();
    assert_eq!(chat[0].calls.len(), 1);
    assert_eq!(chat[1].answer, "a");
    assert!(chat[1].text.contains("interrupted"));
    assert!(messages(&json!({"messages":[call,{"role":"tool","tool_call_id":"a","content":"first"},{"role":"tool","tool_call_id":"a","content":"duplicate"}]})).is_err());
}
#[test]
fn actual_magpie_tool_image_fixture_attaches_image_to_original_result() {
    let chat: Value = serde_json::from_str(include_str!("chat-tool-image.json")).unwrap();
    let (msgs, _) = messages(&chat).unwrap();
    assert_eq!(
        msgs.iter().map(|m| m.role).collect::<Vec<_>>(),
        vec![1, 2, 4]
    );
    assert_eq!(msgs[2].answer, "call_view1");
    assert_eq!(msgs[2].text, "(no output)");
    assert_eq!(msgs[2].images.len(), 1);
    assert_eq!(msgs[2].images[0].mime, "image/png");
}
#[test]
fn direct_tool_images_keep_their_original_call_owner() {
    let chat = json!({"messages":[{"role":"assistant","tool_calls":[{"id":"a","function":{"name":"shot","arguments":"{}"}}]},{"role":"tool","tool_call_id":"a","content":[{"type":"text","text":"result"},{"type":"image_url","image_url":{"url":"data:image/png;base64,YQ=="}}]}]});
    let (msgs, _) = messages(&chat).unwrap();
    assert_eq!(msgs[1].answer, "a");
    assert_eq!(msgs[1].images[0].data, "YQ==");
}
#[test]
fn unsupported_urls_files_audio_and_bad_base64_are_explicit_errors() {
    for part in [
        json!({"type":"image_url","image_url":{"url":"https://example.test/image.png"}}),
        json!({"type":"image_url","image_url":{"url":"data:image/png;base64,%%%"}}),
        json!({"type":"input_file","file_url":"file:///private/local.pdf"}),
        json!({"type":"input_audio","data":"YQ=="}),
    ] {
        assert!(messages(&json!({"messages":[{"role":"user","content":[part]}]})).is_err());
    }
}
#[test]
fn instructions_response_format_and_tools_are_preserved_in_native_request() {
    let chat = json!({"messages":[{"role":"system","content":"Original system"},{"role":"developer","content":"Original developer"},{"role":"user","content":"Question"}],"response_format":{"type":"json_object"},"tools":[{"type":"function","function":{"name":"original_name","description":"Original description","parameters":{"type":"object","properties":{"x":{"type":"string"}}}}}]});
    let bytes = build(&chat, "swe-2-high", "private-key").unwrap();
    let fields = fields(&bytes).unwrap();
    let message = fields.iter().find(|f| f.key == 3).unwrap();
    let text = crate::protobuf::fields(message.data)
        .unwrap()
        .into_iter()
        .find(|f| f.key == 3)
        .unwrap()
        .text()
        .unwrap()
        .to_owned();
    assert!(text.contains("Original system\n\nOriginal developer"));
    assert!(text.contains("Respond with a single JSON object"));
    assert!(text.contains("Original description"));
    assert!(text.ends_with("Question"));
    let tool = fields.iter().find(|f| f.key == 10).unwrap();
    let parts = crate::protobuf::fields(tool.data).unwrap();
    assert_eq!(parts[0].text().unwrap(), "original_name");
    assert!(parts[1].text().unwrap().contains("Described under"));
    assert!(parts[2].text().unwrap().contains("properties"));
}
#[test]
fn sampling_zero_is_the_native_near_greedy_value() {
    let bytes = build(
        &json!({"messages":[{"role":"user","content":"hi"}],"temperature":0,"top_p":0}),
        "swe-2-high",
        "key",
    )
    .unwrap();
    let fs = fields(&bytes).unwrap();
    let opts = fs.iter().find(|f| f.key == 8).unwrap();
    let opts = fields(opts.data).unwrap();
    for field in opts.iter().filter(|f| [5, 8].contains(&f.key)) {
        assert_eq!(f64::from_le_bytes(field.data.try_into().unwrap()), 0.000001);
    }
}
#[test]
fn protocol_origin_and_stops_are_validated_before_any_account_request() {
    let mut request = fixture::fetch(json!({"model":"swe-2","messages":[]}));
    request.url = "https://evil.test/devin/v1/chat/completions".into();
    assert!(parse(&request).is_err());
    let request = fixture::fetch(json!({"model":"swe-2","messages":[],"stop":["END"]}));
    assert!(parse(&request).is_err());
    let request = fixture::fetch(json!({"model":"swe-2","messages":[],"top_p":1.1}));
    assert!(parse(&request).is_err());
}
