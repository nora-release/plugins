use super::*;
use crate::tests::collect;

fn chunks(body: &[u8]) -> Vec<Value> {
    std::str::from_utf8(body)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("data: "))
        .filter(|data| *data != "[DONE]")
        .map(|data| serde_json::from_str(data).unwrap())
        .collect()
}

fn calls(message: &Value) -> Vec<Value> {
    message["tool_calls"]
        .as_array()
        .into_iter()
        .flatten()
        .map(|call| {
            json!([
                call["function"]["name"],
                serde_json::from_str::<Value>(call["function"]["arguments"].as_str().unwrap())
                    .unwrap()
            ])
        })
        .collect()
}

#[tokio::test]
async fn xml_calls_reach_the_agent_in_streaming_and_completed_responses() {
    for stream in [true, false] {
        for size in [0, 1, 7, 64] {
            let response = ask(&pieces(CALLS, size), tools(), stream).await;
            assert_eq!(response.status, 200);
            let body = collect(response).await;
            if !stream {
                let out: Value = serde_json::from_slice(&body).unwrap();
                let choice = &out["choices"][0];
                assert_eq!(calls(&choice["message"]), expected_calls());
                assert_eq!(choice["message"]["content"].as_str().unwrap_or(""), "");
                assert_eq!(choice["finish_reason"], "tool_calls");
                continue;
            }
            let out = chunks(&body);
            let actual: Vec<_> = out
                .iter()
                .flat_map(|c| calls(&c["choices"][0]["delta"]))
                .collect();
            assert_eq!(actual, expected_calls());
            assert!(out.iter().all(|c| c["choices"][0]["delta"]["content"]
                .as_str()
                .unwrap_or("")
                .is_empty()));
            let finish: Vec<_> = out
                .iter()
                .filter_map(|c| c["choices"][0]["finish_reason"].as_str())
                .collect();
            assert_eq!(finish, ["tool_calls"]);
        }
    }
}

#[tokio::test]
async fn xml_string_parameters_remain_strings_in_bare_and_wrapped_calls() {
    for text in [
        STRING_CALL.to_owned(),
        format!("<tool_call>{STRING_CALL}</tool_call>"),
    ] {
        let response = ask(&pieces(&text, 1), tools(), false).await;
        assert_eq!(response.status, 200);
        let out: Value = serde_json::from_slice(&collect(response).await).unwrap();
        assert_eq!(
            calls(&out["choices"][0]["message"]),
            vec![json!(["bash", {"command":"[1, 2]"}])]
        );
    }
}

#[tokio::test]
async fn xml_truncation_preserves_prior_calls_but_never_finishes_successfully() {
    let first =
        "<invoke name=\"image_result\"><parameter name=\"id\">image-24</parameter></invoke>";
    for (size, suffix) in [0, 1, 6]
        .into_iter()
        .flat_map(|size| [(size, ""), (size, STRING_CALL)])
    {
        let text = format!("Working.\n{first}{CUT}{suffix}");
        let response = ask(&pieces(&text, size), tools(), true).await;
        assert_eq!(response.status, 200);
        let body = collect(response).await;
        let out = chunks(&body);
        assert_eq!(
            out.iter()
                .flat_map(|c| calls(&c["choices"][0]["delta"]))
                .collect::<Vec<_>>(),
            vec![expected_calls()[0].clone()]
        );
        let content: String = out
            .iter()
            .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
            .collect();
        assert_eq!(content, "Working.\n");
        assert!(out.iter().any(|c| c["error"]["message"]
            .as_str()
            .is_some_and(|m| m.contains("cut short"))));
        assert!(out
            .iter()
            .all(|c| c["choices"][0]["finish_reason"].is_null()));
        let response = ask(&pieces(&text, size), tools(), false).await;
        assert_eq!(response.status, 502);
    }
}

#[tokio::test]
async fn xml_incomplete_first_call_is_a_failed_http_response() {
    for stream in [false, true] {
        for size in [0, 3] {
            let response = ask(&pieces(CUT, size), tools(), stream).await;
            assert_eq!(response.status, 502);
            let out: Value = serde_json::from_slice(&collect(response).await).unwrap();
            assert!(out["error"]["message"]
                .as_str()
                .is_some_and(|m| m.contains("cut short")));
            assert!(out.get("choices").is_none());
        }
    }
}

#[tokio::test]
async fn xml_quoted_undeclared_and_no_tool_replies_remain_text() {
    for text in plain_text().into_iter().chain([CALLS.to_owned()]) {
        let declared = if text == CALLS { Vec::new() } else { tools() };
        let response = ask(&pieces(&text, 3), declared, false).await;
        assert_eq!(response.status, 200);
        let out: Value = serde_json::from_slice(&collect(response).await).unwrap();
        assert_eq!(out["choices"][0]["message"]["content"], text);
        assert!(calls(&out["choices"][0]["message"]).is_empty());
        assert_eq!(out["choices"][0]["finish_reason"], "stop");
    }
}
