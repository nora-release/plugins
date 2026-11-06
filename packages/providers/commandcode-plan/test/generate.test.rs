use super::*;
fn upstream(lines: &[Value]) -> nora_ai::HttpStreamingResponse {
    let mut bytes = Vec::new();
    for value in lines {
        bytes.extend(serde_json::to_vec(value).unwrap());
        bytes.push(b'\n');
    }
    let chunks: Vec<_> = bytes
        .chunks(3)
        .map(|bytes| Ok(bytes.to_vec().into()))
        .collect();
    nora_ai::HttpStreamingResponse {
        status: 200,
        headers: BTreeMap::new(),
        body: Box::pin(futures::stream::iter(chunks)),
    }
}
async fn decode(response: FetchResponse) -> Value {
    let bytes = response
        .body
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap()
        .concat();
    serde_json::from_slice(&bytes).unwrap()
}
#[test]
fn chat_tools_images_reasoning_and_grouped_results_follow_the_cli_wire_contract() {
    let value=request(&json!({"model":"deepseek/deepseek-v4-pro","max_tokens":999999,"reasoning_effort":"medium","messages":[{"role":"system","content":"System"},{"role":"user","content":[{"type":"text","text":"Look"},{"type":"image_url","image_url":{"url":"data:image/png;base64,aA=="}}]},{"role":"assistant","reasoning_content":"Think","tool_calls":[{"id":"one","function":{"name":"read","arguments":"{\"path\":\"a\"}"}},{"id":"two","function":{"name":"read","arguments":"{}"}}]},{"role":"tool","tool_call_id":"one","content":"first"},{"role":"tool","tool_call_id":"two","content":"second"}],"tools":[{"type":"function","function":{"name":"read","parameters":{"type":"object"}}}]}),Some("/selected/project"),true).unwrap();
    assert_eq!(value["params"]["max_tokens"], 200000);
    assert_eq!(value["params"]["reasoning_effort"], "high");
    assert_eq!(
        value["params"]["messages"][2]["content"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(
        value["params"]["messages"][2]["content"][0]["toolName"],
        "read"
    );
    assert_eq!(
        value["params"]["messages"][0]["content"][1]["mimeType"],
        "image/png"
    );
    assert_eq!(value["config"]["workingDir"], "/selected/project");
    assert_eq!(value["config"]["isGitRepo"], true);
    assert!(value["config"]["structure"].as_array().unwrap().is_empty());
}
#[test]
fn unsupported_go_sampling_and_file_inputs_are_explicit_errors() {
    for extra in [
        json!({"top_p":0.5}),
        json!({"stop":["end"]}),
        json!({"messages":[{"role":"user","content":[{"type":"file","file":{"file_id":"file"}}]}]}),
    ] {
        let mut chat = json!({"model":"gpt-6-luna","messages":[]});
        chat.as_object_mut()
            .unwrap()
            .extend(extra.as_object().unwrap().clone());
        assert!(request(&chat, None, false).is_err());
    }
    assert_eq!(fit_effort("medium", &["low".into(), "high".into()]), "high");
}
#[tokio::test]
async fn jsonl_conversion_retains_whitespace_tools_and_actual_cache_usage() {
    let lines = [
        json!({"type":"text-delta","text":"Hello "}),
        json!({"type":"text-delta","text":" 世界"}),
        json!({"type":"tool-call","toolCallId":"call","toolName":"read","input":{"path":"a"}}),
        json!({"type":"cache-write-tokens","cacheWriteTokens":20}),
        json!({"type":"finish","finishReason":"stop","totalUsage":{"inputTokens":100,"outputTokens":30,"inputTokenDetails":{"cacheReadTokens":10,"cacheWriteTokens":5},"outputTokenDetails":{"reasoningTokens":3}}}),
    ];
    let value = decode(
        super::response(upstream(&lines), &json!({"model":"chosen","stream":false}))
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(value["choices"][0]["message"]["content"], "Hello  世界");
    assert_eq!(
        value["choices"][0]["message"]["tool_calls"][0]["function"]["name"],
        "read"
    );
    assert_eq!(value["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        value["usage"]["prompt_tokens_details"]["cache_write_tokens"],
        20
    );
    assert_eq!(value["usage"]["total_tokens"], 130);
}
#[tokio::test]
async fn streamed_replies_have_a_real_terminal_and_keep_native_failure_codes() {
    let response=super::response(upstream(&[json!({"type":"error","error":{"message":"MODEL_NOT_IN_PLAN: upgrade","statusCode":500}})]),&json!({"model":"chosen","stream":true})).await.unwrap();
    assert_eq!(response.status, 403);
    let response=super::response(upstream(&[json!({"type":"text-delta","text":"Hello"}),json!({"type":"finish","finishReason":"length","totalUsage":{"inputTokens":2,"outputTokens":1}})]),&json!({"model":"chosen","stream":true})).await.unwrap();
    let bytes = response
        .body
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap()
        .concat();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("\"finish_reason\":\"length\""));
    assert!(text.contains("\"total_tokens\":3"));
    assert!(text.ends_with("data: [DONE]\n\n"));
}
#[tokio::test]
async fn truncated_nonstream_output_is_an_error_while_visible_stream_explains_the_cut() {
    let lines = [json!({"type":"text-delta","text":"partial"})];
    assert_eq!(
        super::response(upstream(&lines), &json!({"model":"chosen","stream":false}))
            .await
            .unwrap()
            .status,
        502
    );
    let response = super::response(upstream(&lines), &json!({"model":"chosen","stream":true}))
        .await
        .unwrap();
    let bytes = response
        .body
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap()
        .concat();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("cut short"));
    assert!(text.contains("partial"));
    assert!(text.ends_with("data: [DONE]\n\n"));
}

async fn streamed_frames(lines: &[Value]) -> Vec<Value> {
    let response = super::response(upstream(lines), &json!({"model":"chosen","stream":true}))
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    let bytes = response
        .body
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap()
        .concat();
    let text = String::from_utf8(bytes).unwrap();
    assert_eq!(text.matches("data: [DONE]\n\n").count(), 1);
    assert!(text.ends_with("data: [DONE]\n\n"));
    let frames: Vec<Value> = text
        .split("\n\n")
        .filter_map(|frame| {
            let data = frame.strip_prefix("data: ")?;
            if data == "[DONE]" {
                None
            } else {
                Some(serde_json::from_str(data).unwrap())
            }
        })
        .collect();
    assert!(frames.iter().all(|frame| frame.get("error").is_none()));
    frames
}

#[tokio::test]
async fn a_complete_tool_call_survives_eof_without_a_finish_line() {
    let frames = streamed_frames(&[
        json!({"type":"tool-call","toolCallId":"c1","toolName":"read","input":{"path":"a"}}),
    ])
    .await;
    assert_eq!(
        frames.last().unwrap()["choices"][0]["finish_reason"],
        "tool_calls"
    );
    let calls: Vec<&Value> = frames
        .iter()
        .flat_map(|frame| {
            frame["choices"][0]["delta"]["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
        })
        .collect();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["id"], "c1");
    assert_eq!(calls[0]["function"]["name"], "read");
    assert_eq!(
        serde_json::from_str::<Value>(calls[0]["function"]["arguments"].as_str().unwrap()).unwrap(),
        json!({"path":"a"})
    );
}

#[tokio::test]
async fn reasoning_only_eof_keeps_reasoning_and_has_no_leading_blank_line_in_the_note() {
    let frames = streamed_frames(&[json!({"type":"reasoning-delta","text":"hm"})]).await;
    let collect = |key: &str| {
        frames
            .iter()
            .filter_map(|frame| frame["choices"][0]["delta"][key].as_str())
            .collect::<String>()
    };
    assert_eq!(collect("reasoning_content"), "hm");
    assert_eq!(collect("content"), "(The reply was cut short: Command Code closed the stream before it finished. Ask again to go on.)");
    assert_eq!(
        frames.last().unwrap()["choices"][0]["finish_reason"],
        "stop"
    );
}
