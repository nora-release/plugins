use super::*;
use crate::tests::{catalog, configured, credentials, env, Client};
use base64::{engine::general_purpose::STANDARD, Engine};
#[path = "../../runtime/test/xml_fixtures.rs"]
mod fixtures;
use fixtures::*;

async fn ask(pieces: &[String], tools: Vec<Value>, stream: bool) -> FetchResponse {
    let client = client(pieces);
    let provider = configured(Site::International, client.clone());
    let body = json!({"model":"model","messages":[{"role":"user","content":"hi"}],"tools":tools,"stream":stream});
    let request = serde_json::from_value(json!({"account":"member","credentials":credentials(),"method":"POST","url":"https://api3.qoder.sh/chat/completions","body":STANDARD.encode(body.to_string())})).unwrap();
    let out = crate::request::fetch(&provider, request).await.unwrap();
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    out
}

fn client(pieces: &[String]) -> Arc<Client> {
    let mut raw: String = pieces
        .iter()
        .map(|piece| env(json!({"choices":[{"delta":{"content":piece}}]})))
        .collect();
    raw.push_str(&env(
        json!({"choices":[{"delta":{},"finish_reason":"stop"}]}),
    ));
    raw.push_str(&env(json!("[DONE]")));
    Client::new(vec![
        (
            200,
            json!({"app":catalog()["app"],"qwork":catalog()["app"]}),
        ),
        (200, json!({"raw":raw,"split":true})),
    ])
}

#[path = "../../runtime/test/xml_response_cases.rs"]
mod response_cases;

fn read(pieces: &[String], tools: Vec<Value>) -> Observed {
    let mut parser = Splitter::new(tools);
    let mut result = Observed::default();
    for piece in pieces.iter().map(Some).chain([None]) {
        let out = match piece {
            Some(piece) => parser.feed(piece),
            None => parser.flush(),
        };
        for part in out {
            match part {
                Piece::Text(text) => result.text.push_str(&text),
                Piece::Call { name, args } => result
                    .calls
                    .push(json!([name, serde_json::from_str::<Value>(&args).unwrap()])),
                Piece::Error(message) => {
                    result.error = Some(message.into());
                    return result;
                }
            }
        }
    }
    result
}

#[path = "../../runtime/test/xml_parser_cases.rs"]
mod parser_cases;

#[path = "../../runtime/test/xml_protocol.rs"]
mod protocol;

#[tokio::test]
async fn xml_all_qoder_sites_dispatch_calls_and_errors_through_the_locked_sdk() {
    for site in [
        Site::International,
        Site::China,
        Site::QwenWork,
        Site::QwenWorkEnterprise,
    ] {
        let manifest: Value = serde_json::from_str(match site {
            Site::International => include_str!("../nora-provider.json"),
            Site::China => include_str!("../../qoder-cn/nora-provider.json"),
            Site::QwenWork => include_str!("../../qwenwork/nora-provider.json"),
            Site::QwenWorkEnterprise => {
                include_str!("../../qwenwork-enterprise/nora-provider.json")
            }
        })
        .unwrap();
        for error in [false, true] {
            let input = if error {
                format!("{CALLS}{CUT}{STRING_CALL}")
            } else {
                CALLS.into()
            };
            let client = client(&pieces(&input, 0));
            let provider = configured(site, client.clone());
            let mut credentials = credentials();
            if site.is_work() {
                credentials["deviceChat"] = json!(true);
                credentials["access"] = json!("device-one");
            }
            if site == Site::QwenWorkEnterprise {
                credentials["enterpriseHost"] = json!("acme.example.com");
                credentials["vpc"] = json!("acme.example.com");
            }
            let url = format!(
                "{}/v1/chat/completions",
                provider.deployment(&credentials).unwrap().api
            );
            let (status, out) = protocol::fetch(provider, manifest.clone(), credentials, url,
                json!({"model":"model","messages":[{"role":"user","content":"hi"}],"stream":true,"tools":tools()})).await;
            assert_eq!(status, 200);
            protocol::assert_calls(&out, &expected_calls(), error);
            assert_eq!(client.requests.lock().unwrap().len(), 2);
        }
    }
}

#[test]
fn xml_complete_calls_are_emitted_before_the_wrapper_and_stream_end() {
    let mut parser = Splitter::new(tools());
    let out = parser.feed(
        "<function_calls><invoke name=\"bash\"><parameter name=\"command\">ls</parameter></invoke>",
    );
    assert!(
        matches!(out.as_slice(), [Piece::Call {name, args}] if name == "bash" && args == "{\"command\":\"ls\"}")
    );
    assert!(parser.feed("</function_calls>").is_empty());
    assert!(parser.flush().is_empty());
}

#[test]
fn xml_qoder_requires_exact_declared_names_only_for_bare_calls() {
    let input = "<function=Bash><parameter=command>ls</parameter></function>";
    for size in [0, 1, 5] {
        assert_eq!(
            read(&pieces(input, size), tools()),
            Observed {
                text: input.into(),
                ..Observed::default()
            }
        );
        let wrapped = format!("<tool_call>{input}</tool_call>");
        assert_eq!(
            read(&pieces(&wrapped, size), tools()).calls,
            vec![json!(["Bash", {"command":"ls"}])]
        );
        assert_eq!(
            read(
                &pieces(
                    "<tool_call>{\"name\":\"unknown\",\"arguments\":{}}</tool_call>",
                    size
                ),
                Vec::new()
            )
            .calls,
            vec![json!(["unknown", {}])]
        );
    }
}
