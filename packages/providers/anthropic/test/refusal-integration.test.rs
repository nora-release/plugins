use super::*;

#[tokio::test]
async fn cli_refusal_retains_vendor_details_in_streamed_and_whole_replies_with_or_without_schema() {
    let directory = fixture::Directory::new();
    let credentials = fixture::profile(&directory.0.join("claude-accounts/account-a"));
    let mut host = Host::new(&directory.0).await;
    for prompt in ["STREAMED_REFUSAL", "WHOLE_REFUSAL"] {
        for schema in [false, true] {
            for stream in [false, true] {
                let mut request = json!({"model":"claude-test","max_tokens":100,"stream":stream,"messages":[user(prompt)]});
                if schema {
                    request["output_config"] = json!({"format":{"type":"json_schema","schema":{"type":"object","properties":{"answer":{"type":"string"}},"required":["answer"],"additionalProperties":false}}});
                }
                let id = host.send("fetch", json!({"account":"work","credentials":credentials,"networkProxy":{"mode":"direct"},"url":"https://api.anthropic.com/v1/messages","method":"POST","metadata":{"sessionID":format!("{prompt}-{schema}-{stream}"),"turnID":"one"},"body":STANDARD.encode(request.to_string())})).await;
                let (status, body) = host.response(id).await;
                assert_eq!(
                    status, 200,
                    "{prompt} schema={schema} stream={stream}: {body}"
                );
                let (ending, usage) = if stream {
                    let events: Vec<Value> = body
                        .lines()
                        .filter_map(|line| line.strip_prefix("data: "))
                        .map(|line| serde_json::from_str(line).unwrap())
                        .collect();
                    assert_eq!(
                        events
                            .iter()
                            .filter(|event| event["type"] == "message_start")
                            .count(),
                        1,
                        "{body}"
                    );
                    assert_eq!(
                        events
                            .iter()
                            .filter(|event| event["type"] == "message_stop")
                            .count(),
                        1,
                        "{body}"
                    );
                    assert!(
                        !events.iter().any(|event| event["type"] == "error"),
                        "{body}"
                    );
                    let delta = events
                        .iter()
                        .find(|event| event["type"] == "message_delta")
                        .unwrap();
                    (delta["delta"].clone(), delta["usage"].clone())
                } else {
                    let message: Value = serde_json::from_str(&body).unwrap();
                    assert_eq!(message["content"], json!([]));
                    (message.clone(), message["usage"].clone())
                };
                assert_eq!(ending["stop_reason"], "refusal", "{body}");
                assert_eq!(
                    ending["stop_details"],
                    json!({"type":"refusal","category":"cyber","explanation":"flagged"}),
                    "{body}"
                );
                assert_eq!(usage["input_tokens"], 3, "{body}");
                assert_eq!(usage["output_tokens"], 2, "{body}");
            }
        }
    }
}
