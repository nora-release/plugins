use super::*;
use std::collections::BTreeSet;

fn request(wire: Wire, body: Value) -> Request {
    Request {
        wire,
        path: "/generate".into(),
        query: String::new(),
        model: Value::Null,
        body,
        names: BTreeSet::new(),
        fast: false,
    }
}

#[test]
fn responses_preserves_newest_image_calls_outputs_and_non_attachment_fields() {
    let image = json!({"type":"input_image","image_url":"data:image/png;base64,AAAA"});
    let body = json!({"input":[
        {"role":"user","content":[{"type":"input_text","text":"a"},image]},
        {"type":"function_call","call_id":"c1","name":"view_image","arguments":"{}"},
        {"type":"function_call_output","call_id":"c1","output":[image]},
        {"role":"user","content":[image]}
    ],"tools":[{"parameters":{"examples":[image]}}]});
    let mut r = request(Wire::Responses, body.clone());
    assert_eq!(omit_older(&mut r), 2);
    assert_eq!(
        r.body["input"][0]["content"][1],
        json!({"type":"input_text","text":LEFT_OUT})
    );
    assert_eq!(
        r.body["input"][2],
        json!({"type":"function_call_output","call_id":"c1","output":[{"type":"input_text","text":LEFT_OUT}]})
    );
    assert_eq!(r.body["input"][1], body["input"][1]);
    assert_eq!(r.body["input"][3], body["input"][3]);
    assert_eq!(r.body["tools"], body["tools"]);
}

#[test]
fn chat_uses_text_placeholder_and_keeps_latest_file() {
    let image = json!({"type":"image_url","image_url":{"url":"data:image/png;base64,AAAA"}});
    let file = json!({"type":"file","file":{"file_data":"data:application/pdf;base64,BBBB"}});
    let mut r = request(
        Wire::Chat,
        json!({"messages":[{"role":"user","content":[image,{"type":"text","text":"x"}]},{"role":"user","content":[file]}]}),
    );
    assert_eq!(omit_older(&mut r), 1);
    assert_eq!(
        r.body["messages"][0]["content"],
        json!([{"type":"text","text":LEFT_OUT},{"type":"text","text":"x"}])
    );
    assert_eq!(r.body["messages"][1]["content"][0], file);
}

#[test]
fn anthropic_replacements_preserve_cache_markers() {
    let document = json!({"type":"document","source":{"type":"base64","data":"AAAA"},"cache_control":{"type":"ephemeral"}});
    let mut r = request(
        Wire::Anthropic,
        json!({"messages":[{"role":"user","content":[document.clone(),document.clone()]}]}),
    );
    assert_eq!(omit_older(&mut r), 1);
    assert_eq!(
        r.body["messages"][0]["content"][0],
        json!({"type":"text","text":LEFT_OUT,"cache_control":{"type":"ephemeral"}})
    );
    assert_eq!(r.body["messages"][0]["content"][1], document);
}

#[test]
fn gemini_keeps_function_responses_but_removes_only_old_inline_parts() {
    let image = json!({"inlineData":{"mimeType":"image/png","data":"AAAA"}});
    let mut r = request(
        Wire::Gemini,
        json!({"contents":[
            {"role":"user","parts":[image]},
            {"role":"model","parts":[{"functionCall":{"name":"view_image","args":{}}}]},
            {"role":"user","parts":[{"functionResponse":{"name":"view_image","response":{},"parts":[image]}}]},
            {"role":"user","parts":[image]}
        ]}),
    );
    assert_eq!(omit_older(&mut r), 2);
    assert_eq!(r.body["contents"][0]["parts"], json!([{"text":LEFT_OUT}]));
    assert_eq!(
        r.body["contents"][2]["parts"],
        json!([{"functionResponse":{"name":"view_image","response":{},"parts":[]}}])
    );
    assert_eq!(r.body["contents"][3]["parts"], json!([image]));
}

#[test]
fn one_attachment_or_non_array_input_is_left_untouched() {
    for body in [
        json!({"input":"hi"}),
        json!({"input":[{"role":"user","content":[{"type":"input_image","image_url":"data:image/png;base64,AAAA"}]}]}),
    ] {
        let mut r = request(Wire::Responses, body.clone());
        assert_eq!(omit_older(&mut r), 0);
        assert_eq!(r.body, body);
    }
}
