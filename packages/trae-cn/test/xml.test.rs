use super::*;
use crate::tests::{configured, credentials, listing, Client};
use base64::{engine::general_purpose::STANDARD, Engine};
#[path = "../../runtime/test/xml_fixtures.rs"]
mod fixtures;
use fixtures::*;

async fn ask(pieces: &[String], tools: Vec<Value>, stream: bool) -> FetchResponse {
    let client = client(pieces);
    let provider = configured(Site::China, client.clone());
    let body = json!({"model":"model","messages":[{"role":"user","content":"hi"}],"tools":tools,"stream":stream});
    let request = serde_json::from_value(json!({"account":"member","credentials":credentials(),"method":"POST","url":"https://trae-api-cn.mchost.guru/v1/chat/completions","body":STANDARD.encode(body.to_string())})).unwrap();
    let out = crate::request::fetch(&provider, request).await.unwrap();
    assert_eq!(client.requests.lock().unwrap().len(), 2);
    out
}

fn client(pieces: &[String]) -> Arc<Client> {
    let mut raw: String = pieces
        .iter()
        .map(|piece| format!("event: output\ndata: {}\n\n", json!({"response":piece})))
        .collect();
    raw.push_str("event: done\ndata: {\"finish_reason\":\"stop\"}\n\n");
    Client::new(vec![
        (200, listing()),
        (200, json!({"raw":raw,"split":true})),
    ])
}

#[path = "../../runtime/test/xml_response_cases.rs"]
mod response_cases;

fn read(pieces: &[String], tools: Vec<Value>) -> Observed {
    let mut parser = crate::tools::TextTools::new(tools);
    let mut result = Observed::default();
    for (piece, end) in pieces
        .iter()
        .map(|p| (p.as_str(), false))
        .chain([("", true)])
    {
        let part = parser.push(piece, end).unwrap();
        result.text.push_str(&part.text);
        result.calls.extend(part.calls.into_iter().map(|call| {
            json!([
                call.name,
                serde_json::from_str::<Value>(&call.arguments).unwrap()
            ])
        }));
        if let Some(error) = part.error {
            result.error = Some(error.message);
            break;
        }
    }
    result
}

#[path = "../../runtime/test/xml_parser_cases.rs"]
mod parser_cases;

#[path = "../../runtime/test/xml_protocol.rs"]
mod protocol;

#[tokio::test]
async fn xml_both_trae_sites_dispatch_calls_and_errors_through_the_locked_sdk() {
    for site in [Site::China, Site::Global] {
        let manifest: Value = serde_json::from_str(if site == Site::China {
            include_str!("../nora-plugin.json")
        } else {
            include_str!("../../trae-global/nora-plugin.json")
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
            let credentials = credentials();
            let url = format!("{}/v1/chat/completions", site.api(&credentials));
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
    let mut parser = crate::tools::TextTools::new(tools());
    let out = parser.push("<function_calls><invoke name=\"bash\"><parameter name=\"command\">ls</parameter></invoke>", false).unwrap();
    assert_eq!(out.calls.len(), 1);
    assert_eq!(out.calls[0].name, "bash");
    assert!(out.text.is_empty());
    assert!(out.error.is_none());
    let ending = parser.push("</function_calls>", true).unwrap();
    assert!(ending.text.is_empty());
    assert!(ending.calls.is_empty());
    assert!(ending.error.is_none());
}

#[test]
fn xml_trae_aliases_resolve_to_the_first_declared_name_while_exact_names_win() {
    let declared = json!([{"function":{"name":"list_files"}},{"function":{"name":"list-files"}}])
        .as_array()
        .unwrap()
        .clone();
    for (name, expected) in [("ListFiles", "list_files"), ("list-files", "list-files")] {
        let out = read(
            &pieces(
                &format!("<function={name}><parameter=x>1</parameter></function>"),
                1,
            ),
            declared.clone(),
        );
        assert_eq!(out.calls, vec![json!([expected, {"x":1}])]);
        assert!(out.error.is_none());
    }
}
