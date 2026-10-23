use super::*;
use crate::test_support::Client;
use base64::{engine::general_purpose::STANDARD, Engine};
fn account() -> Account {
    Account::inputs(&json!({"endpoint":"https://remote.test","apiKey":"remote-private"})).unwrap()
}
fn request(path: &str, body: &[u8]) -> FetchRequest {
    FetchRequest {
        account: "key".into(),
        credentials: None,
        network_proxy: None,
        url: format!("https://remote.test{path}"),
        method: "POST".into(),
        headers: BTreeMap::from([
            ("authorization".into(), "Bearer caller-secret".into()),
            ("x-api-key".into(), "caller-secret".into()),
            ("cookie".into(), "private-cookie".into()),
            ("content-type".into(), "application/json".into()),
        ]),
        metadata: BTreeMap::new(),
        body: Some(STANDARD.encode(body)),
        body_file: None,
    }
}
#[test]
fn remote_budget_markers_scope_the_key_while_other_remote_failures_scope_only_the_model() {
    for status in [200, 400, 402, 403, 404, 429, 500] {
        let headers = response_headers(status, &BTreeMap::new());
        assert_eq!(headers["X-Nora-Failure-Scope"], "model");
        assert!(!headers.contains_key("X-Nora-Client-Limit"));
    }
    assert_eq!(
        response_headers(401, &BTreeMap::new())["X-Nora-Failure-Scope"],
        "account"
    );
    let headers = response_headers(
        429,
        &BTreeMap::from([("X-Magpie-Limit-Reset".into(), "2026-10-23T00:00:00Z".into())]),
    );
    assert_eq!(headers["X-Nora-Failure-Scope"], "account");
    assert_eq!(headers["X-Nora-Client-Limit"], "true");
    assert_eq!(headers["X-Nora-Reset-At"], "2026-10-23T00:00:00Z");
}
#[test]
fn forwarded_caller_identity_never_overwrites_gateway_authentication_or_replays_private_attestation(
) {
    let mut request = request("/v1/responses", b"{}");
    request.metadata = BTreeMap::from([
        ("agent".into(), "pi".into()),
        ("via".into(), "nora".into()),
        ("sessionID".into(), "chat".into()),
        ("header:user-agent".into(), "codex_cli_rs/1.0".into()),
        ("header:originator".into(), "codex_exec".into()),
        ("header:session_id".into(), "native-session".into()),
        ("header:x-openai-subagent".into(), "thread_title".into()),
        ("header:x-oai-attestation".into(), "private".into()),
        ("header:authorization".into(), "Bearer malicious".into()),
        (
            "header:x-openai-codex-luna-reserve".into(),
            "private-reserve".into(),
        ),
    ]);
    let headers = request_headers(&account(), &request);
    assert_eq!(headers["authorization"], "Bearer remote-private");
    assert_eq!(headers["x-magpie-agent"], "pi");
    assert_eq!(headers["x-magpie-session"], "chat");
    assert_eq!(headers["user-agent"], "codex_cli_rs/1.0");
    assert_eq!(headers["originator"], "codex_exec");
    assert_eq!(headers["session_id"], "native-session");
    assert!(!headers.contains_key("cookie"));
    assert!(!headers.contains_key("x-oai-attestation"));
    assert!(!headers.contains_key("x-openai-codex-luna-reserve"));
}
#[tokio::test]
async fn every_native_and_media_wire_request_is_forwarded_once_without_reencoding_or_renaming() {
    for(path,body)in [("/v1/responses",br#"{"model":"codex/gpt","input":"hi","service_tier":"priority","reasoning":{"effort":"high"}}"#.as_slice()),("/v1/messages",br#"{"model":"claude/opus","messages":[],"max_tokens":50}"#.as_slice()),("/v1/images/generations",br#"{"model":"grok/image","prompt":"kite","n":2}"#.as_slice()),("/v1/embeddings",br#"{"model":"lib/embed","input":["one","two"],"dimensions":3}"#.as_slice()),("/v1/rerank",br#"{"model":"lib/rank","query":"bird","documents":["a","b"],"top_n":2}"#.as_slice()),("/v1/systemone",br#"{"model":"judge/custom","state":{},"questions":{"ok":{"type":"noul"}}}"#.as_slice())]{let client=Client::default();client.raw(200,BTreeMap::from([("content-type".into(),"application/json".into())]),b"{\"ok\":true}".to_vec());let response=fetch(&client,&account(),request(path,body)).await.unwrap();assert_eq!(response.status,200);let sent=client.requests.lock().unwrap();assert_eq!(sent.len(),1);assert_eq!(sent[0].url,format!("https://remote.test{path}"));assert_eq!(sent[0].body.as_deref(),Some(body));assert_eq!(sent[0].headers["authorization"],"Bearer remote-private");}
}
#[tokio::test]
async fn errors_keep_their_status_scope_and_body_but_redact_this_gateways_key() {
    let client = Client::default();
    client.push(403,json!({"error":{"message":"Verify your account to continue. remote-private","type":"verification_required"}}));
    let mut response = fetch(&client, &account(), request("/v1/responses", b"{}"))
        .await
        .unwrap();
    assert_eq!(response.status, 403);
    assert_eq!(response.headers["X-Nora-Failure-Scope"], "model");
    let bytes = response.body.next().await.unwrap().unwrap();
    let text = String::from_utf8(bytes).unwrap();
    assert!(text.contains("Verify your account"));
    assert!(!text.contains("remote-private"));
}
#[tokio::test]
async fn model_discovery_requests_real_native_metadata_and_prices() {
    let client = Client::default();
    client.push(200, json!({"data":[]}));
    read(&client, &account(), HttpMethod::Get, "/v1/models", true)
        .await
        .unwrap();
    let requests = client.requests.lock().unwrap();
    for name in [
        "X-Magpie-Drawers",
        "X-Magpie-Videomakers",
        "X-Magpie-Prices",
        "X-Magpie-Deciders",
    ] {
        assert_eq!(requests[0].headers[name], "1");
    }
}
#[tokio::test]
async fn old_quota_endpoints_and_restricted_keys_are_identified_without_fabricated_allowances() {
    let client = Client::default();
    client.raw(404, BTreeMap::new(), b"old gateway".to_vec());
    let error = read(
        &client,
        &account(),
        HttpMethod::Get,
        "/v1/magpie/quotas/cards",
        false,
    )
    .await
    .unwrap_err();
    assert!(error.message.contains("Update"));
    client.push(403, json!({"error":{"code":"gateway_key_held"}}));
    assert!(
        read(
            &client,
            &account(),
            HttpMethod::Get,
            "/v1/magpie/quotas/cards",
            false
        )
        .await
        .unwrap_err()
        .held
    );
}

#[tokio::test]
async fn a_real_multipart_body_file_is_streamed_unchanged_to_the_explicit_remote() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let directory = crate::test_support::Directory::new();
    let file = directory.0.join("upload");
    let body=b"--boundary\r\nContent-Disposition: form-data; name=\"model\"\r\n\r\nrelay/image\r\n--boundary\r\nContent-Disposition: form-data; name=\"image\"; filename=\"input.png\"\r\nContent-Type: image/png\r\n\r\n\x00\xffimage\r\n--boundary--\r\n";
    std::fs::write(&file, body).unwrap();
    let descriptor = nora_provider_plugin_sdk::RequestBodyFile::from_path(file).unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        let header_end = loop {
            let mut chunk = [0u8; 4096];
            let count = socket.read(&mut chunk).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&chunk[..count]);
            if let Some(index) = bytes.windows(4).position(|bytes| bytes == b"\r\n\r\n") {
                break index + 4;
            }
        };
        let headers = String::from_utf8(bytes[..header_end].to_vec()).unwrap();
        let length = headers
            .lines()
            .filter_map(|line| line.split_once(':'))
            .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
            .unwrap()
            .1
            .trim()
            .parse::<usize>()
            .unwrap();
        while bytes.len() < header_end + length {
            let mut chunk = [0u8; 4096];
            let count = socket.read(&mut chunk).await.unwrap();
            assert!(count > 0);
            bytes.extend_from_slice(&chunk[..count]);
        }
        let reply = b"{\"data\":[{\"b64_json\":\"aGk=\"}]}";
        socket.write_all(format!("HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",reply.len()).as_bytes()).await.unwrap();
        socket.write_all(reply).await.unwrap();
        (headers, bytes[header_end..].to_vec())
    });
    let account =
        Account::inputs(&json!({"endpoint":format!("http://{address}"),"apiKey":"remote-private"}))
            .unwrap();
    let mut outgoing = request("/v1/images/edits", b"");
    outgoing.url = account.at("/v1/images/edits");
    outgoing.body = None;
    outgoing.body_file = Some(descriptor);
    outgoing.headers.insert(
        "content-type".into(),
        "multipart/form-data; boundary=boundary".into(),
    );
    let mut response = fetch(&Client::default(), &account, outgoing).await.unwrap();
    assert_eq!(response.status, 200);
    while let Some(chunk) = response.body.next().await {
        chunk.unwrap();
    }
    let (headers, received) = server.await.unwrap();
    assert_eq!(received, body);
    assert!(headers
        .to_ascii_lowercase()
        .contains("authorization: bearer remote-private"));
    assert!(!headers.contains("caller-secret"));
    assert!(headers.contains("boundary=boundary"));
}
#[tokio::test]
async fn changed_body_files_are_rejected_before_any_request_can_send() {
    let directory = crate::test_support::Directory::new();
    let path = directory.0.join("upload");
    std::fs::write(&path, b"original").unwrap();
    let descriptor = nora_provider_plugin_sdk::RequestBodyFile::from_path(&path).unwrap();
    std::fs::write(path, b"modified").unwrap();
    let mut outgoing = request("/v1/images/edits", b"");
    outgoing.body = None;
    outgoing.body_file = Some(descriptor);
    assert!(fetch(&Client::default(), &account(), outgoing)
        .await
        .is_err());
}
#[tokio::test]
async fn video_status_and_binary_content_keep_the_remote_id_range_and_content_type() {
    let client = Client::default();
    client.raw(
        206,
        BTreeMap::from([
            ("content-type".into(), "video/mp4".into()),
            ("content-range".into(), "bytes 0-2/3".into()),
        ]),
        vec![0, 255, 1],
    );
    let mut outgoing = request("/v1/videos/remote-job-id/content", b"");
    outgoing.method = "GET".into();
    outgoing.body = None;
    outgoing.headers.insert("range".into(), "bytes=0-2".into());
    let mut response = fetch(&client, &account(), outgoing).await.unwrap();
    assert_eq!(response.status, 206);
    assert_eq!(response.headers["content-type"], "video/mp4");
    assert_eq!(
        response.body.next().await.unwrap().unwrap(),
        vec![0, 255, 1]
    );
    assert_eq!(
        client.requests.lock().unwrap()[0].headers["range"],
        "bytes=0-2"
    );
    assert!(client.requests.lock().unwrap()[0]
        .url
        .ends_with("/v1/videos/remote-job-id/content"));
}
