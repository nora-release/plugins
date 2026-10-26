use super::*;

#[path = "tool_search_protocol.test.rs"]
mod protocol;

struct WireReply {
    status: u16,
    content_type: &'static str,
    chunks: Vec<Result<Vec<u8>, &'static str>>,
    stay_open: bool,
}

fn response(bytes: &[u8], content_type: &'static str, cut: usize) -> WireReply {
    WireReply {
        status: 200,
        content_type,
        chunks: bytes.chunks(cut).map(|bytes| Ok(bytes.to_vec())).collect(),
        stay_open: false,
    }
}

struct WireClient {
    requests: StdMutex<Vec<HttpRequest>>,
    replies: StdMutex<VecDeque<WireReply>>,
}

#[async_trait]
impl HttpClient for WireClient {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        assert!(
            request.url.ends_with("/v1/messages")
                || request.url.ends_with("/v1/messages/count_tokens"),
            "Unexpected request {}",
            request.url
        );
        self.requests.lock().unwrap().push(request);
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .expect("Unexpected request");
        let stream = futures::stream::iter(reply.chunks.into_iter().map(|chunk| {
            chunk
                .map(Into::into)
                .map_err(|message| HttpClientError::Transport(message.into()))
        }));
        let stream: nora_ai::HttpByteStream = if reply.stay_open {
            Box::pin(stream.chain(futures::stream::pending()))
        } else {
            Box::pin(stream)
        };
        Ok(HttpStreamingResponse {
            status: reply.status,
            headers: BTreeMap::from([
                ("Content-Type".into(), reply.content_type.into()),
                ("content-length".into(), "999999".into()),
                ("x-fixture".into(), "kept".into()),
            ]),
            body: stream,
        })
    }
}

fn configured(replies: Vec<WireReply>) -> (Zcode, Arc<WireClient>, Arc<Store>) {
    let client = Arc::new(WireClient {
        requests: StdMutex::new(Vec::new()),
        replies: StdMutex::new(replies.into()),
    });
    let store = Arc::new(Store {
        saved: StdMutex::new(Vec::new()),
    });
    let mut plugin = Zcode::default();
    plugin.core = Core::with_dependencies("zcode", client.clone(), store.clone());
    plugin
        .core
        .initialize(InitializeParams {
            subscription_accounts_enabled: true,
            installed_manifest: None,
            runtime_config: Value::Null,
            api_version: 1,
            plugin_id: "zcode".into(),
            plugin_directory: PathBuf::new(),
            data_directory: PathBuf::new(),
            manifest: serde_json::from_str(include_str!("../nora-provider.json")).unwrap(),
        })
        .unwrap();
    (plugin, client, store)
}

fn claude_code() -> Value {
    json!({
        "model":"GLM-5.3-Flash", "max_tokens":32000, "stream":true, "top_p":0.8,
        "system":[{"type":"text","text":"ToolSearch is available; keep this literal name."}],
        "metadata":{"user_id":"caller-device","trace":"kept"},
        "messages":[
            {"role":"user","content":[{"type":"text","text":"Call ToolSearch then Bash."}]},
            {"role":"system","content":[{"type":"text","text":"The following deferred tools are now available via ToolSearch.\nmcp__probe__probe_0"}]},
            {"role":"assistant","content":[{"type":"tool_use","id":"toolu_s","name":"ToolSearch","input":{"query":"ToolSearch","max_results":5,"nested":{"name":"ToolSearch"}}},{"type":"text","text":"ToolSearch"}]},
            {"role":"user","content":[{"type":"tool_result","tool_use_id":"toolu_s","content":[{"type":"tool_reference","tool_name":"mcp__probe__probe_0"}]}]}
        ],
        "tool_choice":{"type":"tool","name":"ToolSearch","disable_parallel_tool_use":true},
        "tools":[
            {"name":"Agent","input_schema":{"type":"object"}},
            {"name":"Bash","input_schema":{"type":"object"}},
            {"name":"Read","input_schema":{"type":"object"}},
            {"name":"ToolSearch","description":"ToolSearch","input_schema":{"type":"object","properties":{"query":{"type":"string","description":"ToolSearch"}}}},
            {"name":"DeferredToolPlaceholder","defer_loading":true,"input_schema":{"type":"object"}},
            {"name":"Write","input_schema":{"type":"object"},"defer_loading":false},
            {"name":"mcp__probe__probe_0","input_schema":{"type":"object"},"defer_loading":true}
        ]
    })
}

fn fetch_request(stored: Value, body: &Value) -> FetchRequest {
    let mut request = request(stored, "GLM-5.3-Flash");
    request.body =
        Some(base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(body).unwrap()));
    request.headers.insert(
        "anthropic-beta".into(),
        "advanced-tool-use-2025-11-20".into(),
    );
    request
}

fn tool_answer() -> Value {
    json!({"type":"message","role":"assistant","content":[
        {"type":"text","text":"ToolSearchTool"},
        {"type":"tool_use","id":"toolu_1","name":"ToolSearchTool","input":{"query":"ToolSearchTool","nested":{"name":"ToolSearchTool"}}},
        {"type":"tool_use","id":"toolu_2","name":"Bash","input":{"command":"echo ToolSearchTool"}},
        {"type":"tool_use","id":"toolu_3","name":"ToolSearchTool","input":{"query":"select:Read"}},
        {"type":"server_tool_use","id":"toolu_4","name":"ToolSearchTool","input":{}}
    ],"stop_reason":"tool_use","usage":{"input_tokens":7,"output_tokens":9}})
}

fn restored_answer() -> Value {
    let mut answer = tool_answer();
    answer["content"][1]["name"] = json!("ToolSearch");
    answer["content"][3]["name"] = json!("ToolSearch");
    answer
}

fn assert_searchless(sent: &Value) {
    assert_eq!(
        sent["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tool| tool["name"].as_str().unwrap())
            .collect::<Vec<_>>(),
        [
            "Agent",
            "Bash",
            "Read",
            "ToolSearchTool",
            "Write",
            "mcp__probe__probe_0"
        ]
    );
    assert!(sent["tools"]
        .as_array()
        .unwrap()
        .iter()
        .all(|tool| tool.get("defer_loading").is_none()));
    let call = sent["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .find(|block| block["id"] == "toolu_s")
        .unwrap();
    assert_eq!(call["name"], "ToolSearchTool");
    assert_eq!(
        call["input"],
        claude_code()["messages"][2]["content"][0]["input"]
    );
    let result = sent["messages"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|message| message["content"].as_array().into_iter().flatten())
        .find(|block| block["type"] == "tool_result")
        .unwrap();
    assert_eq!(result["tool_use_id"], "toolu_s");
    assert_eq!(
        result["content"],
        claude_code()["messages"][3]["content"][0]["content"]
    );
    assert_eq!(
        sent["tool_choice"],
        json!({"type":"tool","name":"ToolSearchTool","disable_parallel_tool_use":true})
    );
}

async fn bytes(response: FetchResponse) -> Vec<u8> {
    response
        .body
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap()
        .concat()
}

#[tokio::test]
async fn coding_requests_on_both_sites_offer_all_tools_and_restore_only_actual_calls() {
    for selected in [Site::Zai, Site::BigModel] {
        let answer = serde_json::to_vec(&tool_answer()).unwrap();
        let (plugin, client, store) = configured(vec![response(&answer, "application/json", 7)]);
        let mut stored = credentials("coding.key", "");
        stored["state"]["site"] = json!(selected.id());
        let response = plugin
            .fetch(fetch_request(stored, &claude_code()))
            .await
            .unwrap();
        assert_eq!(response.status, 200);
        assert_eq!(response.headers["x-fixture"], "kept");
        assert!(!response.headers.contains_key("content-length"));
        assert_eq!(decode(response).await, restored_answer());
        let requests = client.requests.lock().unwrap();
        let sent: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
        assert_searchless(&sent);
        assert_eq!(sent["system"], claude_code()["system"]);
        assert_eq!(sent["metadata"], claude_code()["metadata"]);
        assert_eq!(sent["top_p"], 0.8);
        assert_eq!(requests[0].url, format!("{}/v1/messages", selected.base()));
        assert_eq!(
            requests[0].headers["anthropic-beta"],
            "advanced-tool-use-2025-11-20"
        );
        assert!(store.saved.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn start_and_personal_trial_requests_share_the_conversion_without_paid_fallback() {
    for trial in [false, true] {
        let answer = serde_json::to_vec(&tool_answer()).unwrap();
        let (plugin, client, store) = configured(vec![response(&answer, "application/json", 5)]);
        let token = jwt();
        let stored = credentials(if trial { "team.key" } else { "" }, &token);
        if trial {
            cache_route(
                &plugin,
                &stored["state"],
                readings::gift(&gift_balance(), false),
            )
            .await;
        }
        let mut body = claude_code();
        if trial {
            body["model"] = json!("GLM-5.3-Flash-Trial");
        }
        assert_eq!(
            decode(plugin.fetch(fetch_request(stored, &body)).await.unwrap()).await,
            restored_answer()
        );
        let requests = client.requests.lock().unwrap();
        let sent: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
        assert_searchless(&sent);
        assert_eq!(
            sent["model"].as_str().unwrap().to_ascii_lowercase(),
            "glm-5.3-flash"
        );
        assert_eq!(requests[0].url, format!("{}/v1/messages", endpoint::START));
        assert_eq!(
            requests[0].headers["authorization"],
            format!("Bearer {token}")
        );
        assert!(!requests[0].headers.contains_key("x-api-key"));
        assert!(store.saved.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn absent_search_or_an_existing_alias_keeps_request_bytes_and_native_response_names() {
    for collision in [false, true] {
        let mut body = claude_code();
        if collision {
            body["tools"].as_array_mut().unwrap().push(json!({"name":"ToolSearchTool","defer_loading":true,"input_schema":{"type":"object"}}));
        } else {
            body["tools"]
                .as_array_mut()
                .unwrap()
                .retain(|tool| tool["name"] != "ToolSearch");
        }
        let original = serde_json::to_vec_pretty(&body).unwrap();
        let answer = serde_json::to_vec_pretty(&tool_answer()).unwrap();
        let (plugin, client, _) = configured(vec![response(&answer, "application/json", 3)]);
        let mut req = fetch_request(credentials("coding.key", ""), &body);
        req.body = Some(base64::engine::general_purpose::STANDARD.encode(&original));
        assert_eq!(bytes(plugin.fetch(req).await.unwrap()).await, answer);
        assert_eq!(
            client.requests.lock().unwrap()[0].body.as_ref().unwrap(),
            &original
        );
    }
}

#[tokio::test]
async fn references_text_other_roles_and_non_tool_choices_are_not_rewritten() {
    let mut original = claude_code();
    original["messages"].as_array_mut().unwrap().push(json!({"role":"user","content":[{"type":"tool_use","name":"ToolSearch","input":{}},{"type":"tool_reference","tool_name":"ToolSearch"}]}));
    original["messages"][2]["content"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type":"server_tool_use","name":"ToolSearch","input":{}}));
    original["tool_choice"] = json!({"type":"auto","name":"ToolSearch"});
    let (plugin, client, _) = configured(vec![response(b"{}", "application/json", 2)]);
    plugin
        .fetch(fetch_request(credentials("coding.key", ""), &original))
        .await
        .unwrap();
    let sent: Value =
        serde_json::from_slice(client.requests.lock().unwrap()[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(sent["tools"][3]["name"], "ToolSearchTool");
    assert_eq!(sent["messages"][2]["content"][0]["name"], "ToolSearchTool");
    assert_eq!(
        sent["messages"][2]["content"][2],
        original["messages"][2]["content"][2]
    );
    assert_eq!(sent["messages"][4], original["messages"][4]);
    assert_eq!(sent["tool_choice"], original["tool_choice"]);
}

fn events() -> Vec<Value> {
    vec![
        json!({"type":"message_start","message":{"id":"m1","type":"message","content":[],"usage":{"input_tokens":1}}}),
        json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"我将调用 \"ToolSearchTool\"。"}}),
        json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"ToolSearchTool","input":{}}}),
        json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"query\":\"ToolSearchTool\"}"}}),
        json!({"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"toolu_2","name":"Bash","input":{}}}),
        json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}),
        json!({"type":"message_stop"}),
    ]
}

fn sse(events: &[Value]) -> Vec<u8> {
    events
        .iter()
        .map(|event| {
            format!(
                "event: {}\ndata: {event}\n\n",
                event["type"].as_str().unwrap()
            )
        })
        .collect::<String>()
        .into_bytes()
}

fn read_events(bytes: &[u8]) -> Vec<Value> {
    std::str::from_utf8(bytes)
        .unwrap()
        .lines()
        .filter_map(|line| line.strip_prefix("data:"))
        .map(|data| serde_json::from_str(data).unwrap())
        .collect()
}

#[tokio::test]
async fn sse_tool_calls_are_restored_across_every_small_chunk_boundary() {
    let original = events();
    let encoded = sse(&original);
    let mut expected = original.clone();
    expected[2]["content_block"]["name"] = json!("ToolSearch");
    for cut in [1, 2, 7, 13, encoded.len()] {
        let (plugin, _, _) = configured(vec![response(
            &encoded,
            "text/event-stream; charset=utf-8",
            cut,
        )]);
        let result = bytes(
            plugin
                .fetch(fetch_request(credentials("coding.key", ""), &claude_code()))
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(read_events(&result), expected, "chunk size {cut}");
    }
}

#[tokio::test]
async fn sse_keeps_comments_invalid_records_and_multiline_fragments_and_flushes_a_complete_last_line(
) {
    let prefix = concat!(
        ": ToolSearchTool\r\n",
        "event: error\r\n",
        "data: {\"type\":\"error\",\"error\":{\"message\":\"ToolSearchTool\"}}\r\n\r\n",
        "data: {invalid \"ToolSearchTool\"}\n\n",
        " data: {\"content_block\":{\"type\":\"tool_use\",\"name\":\"ToolSearchTool\"}}\n",
        "data: {\"content_block\":\n",
        "data: {\"type\":\"tool_use\",\"name\":\"ToolSearchTool\"}}\n\n",
        "data: [DONE]\n"
    );
    let line = r#"data:{"type":"content_block_start","content_block":{"type":"tool_use","name":"ToolSearchTool","id":"last","input":{"literal":"ToolSearchTool"}}}"#;
    let original = format!("{prefix}{line}");
    let (plugin, _, _) = configured(vec![response(original.as_bytes(), "text/event-stream", 3)]);
    let result = bytes(
        plugin
            .fetch(fetch_request(credentials("coding.key", ""), &claude_code()))
            .await
            .unwrap(),
    )
    .await;
    let text = std::str::from_utf8(&result).unwrap();
    assert!(text.starts_with(prefix));
    let last: Value = serde_json::from_str(
        text.strip_prefix(prefix)
            .unwrap()
            .strip_prefix("data: ")
            .unwrap(),
    )
    .unwrap();
    assert_eq!(last["content_block"]["name"], "ToolSearch");
    assert_eq!(last["content_block"]["input"]["literal"], "ToolSearchTool");
    assert!(!text.ends_with('\n'));
}

#[tokio::test]
async fn json_without_actual_calls_non_json_and_error_bodies_keep_exact_bytes() {
    for (content_type,original) in [
        ("application/json",b"{ \"error\":{\"message\":\"ToolSearchTool\"}, \"content\": [{\"type\":\"text\",\"text\":\"ToolSearchTool\"}] }".as_slice()),
        ("application/json",b"not JSON ToolSearchTool".as_slice()),
        ("text/plain",b"ToolSearchTool {\"content\":[{\"type\":\"tool_use\",\"name\":\"ToolSearchTool\"}]}".as_slice()),
        ("application/octet-stream",b"\xffToolSearchTool\0".as_slice()),
    ] {
        for status in [200,400,401,403,429,500] {
            let mut reply = response(original,content_type,3);reply.status=status;
            let (plugin,_,store) = configured(vec![reply]);
            let response = plugin.fetch(fetch_request(credentials("coding.key",""),&claude_code())).await.unwrap();
            assert_eq!(response.status,status);
            assert_eq!(bytes(response).await,original);
            assert!(store.saved.lock().unwrap().is_empty());
        }
    }
}

#[tokio::test]
async fn json_content_type_variants_restore_multiple_calls_without_touching_arguments() {
    let original = serde_json::to_vec(&tool_answer()).unwrap();
    let (plugin, _, _) = configured(vec![response(&original, "application/problem+json", 1)]);
    assert_eq!(
        decode(
            plugin
                .fetch(fetch_request(credentials("coding.key", ""), &claude_code()))
                .await
                .unwrap()
        )
        .await,
        restored_answer()
    );
}

#[tokio::test]
async fn a_stream_failure_preserves_completed_calls_and_discards_an_unfinished_line() {
    let complete = sse(&events()[2..3]);
    let mut reply = response(&complete, "text/event-stream", 7);
    reply.chunks.extend([
        Ok(b"data: {\"content_block\":{\"type\":\"tool_use\",\"name\":\"ToolSearchTool\"".to_vec()),
        Err("fixture stream failed"),
    ]);
    let (plugin, _, store) = configured(vec![reply]);
    let response = plugin
        .fetch(fetch_request(credentials("coding.key", ""), &claude_code()))
        .await
        .unwrap();
    let chunks = response.body.collect::<Vec<_>>().await;
    assert!(chunks.last().unwrap().is_err());
    let delivered = chunks
        .into_iter()
        .filter_map(Result::ok)
        .flatten()
        .collect::<Vec<_>>();
    let expected = json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"toolu_1","name":"ToolSearch","input":{}}});
    assert_eq!(read_events(&delivered), [expected]);
    assert!(store.saved.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_completed_sse_record_is_available_before_the_vendor_finishes_the_response() {
    let mut reply = response(&sse(&events()[2..3]), "text/event-stream", 4096);
    reply.stay_open = true;
    let (plugin, _, _) = configured(vec![reply]);
    let mut response = plugin
        .fetch(fetch_request(credentials("coding.key", ""), &claude_code()))
        .await
        .unwrap();
    let first = tokio::time::timeout(Duration::from_millis(100), response.body.next())
        .await
        .expect("The complete event must not wait for EOF")
        .unwrap()
        .unwrap();
    assert_eq!(
        read_events(&first)[0]["content_block"]["name"],
        "ToolSearch"
    );
}

#[tokio::test]
async fn a_json_transport_failure_is_reported_without_emitting_a_partial_message() {
    let mut reply = response(
        b"{\"content\":[{\"type\":\"tool_use\",\"name\":\"ToolSearchTool\"",
        "application/json",
        5,
    );
    reply.chunks.push(Err("fixture stream failed"));
    let (plugin, _, _) = configured(vec![reply]);
    let response = plugin
        .fetch(fetch_request(credentials("coding.key", ""), &claude_code()))
        .await
        .unwrap();
    let chunks = response.body.collect::<Vec<_>>().await;
    assert_eq!(chunks.len(), 1);
    assert!(chunks[0].is_err());
}

#[tokio::test]
async fn a_spent_trial_stays_429_and_never_replays_against_the_coding_plan() {
    let error = br#"{"code":1113,"message":"No resource package","literal":"ToolSearchTool"}"#;
    let (plugin, client, store) = configured(vec![response(error, "application/json", 7)]);
    let stored = credentials("team.key", &jwt());
    cache_route(
        &plugin,
        &stored["state"],
        readings::gift(&gift_balance(), false),
    )
    .await;
    let mut body = claude_code();
    body["model"] = json!("GLM-5.3-Flash-Trial");
    let response = plugin.fetch(fetch_request(stored, &body)).await.unwrap();
    assert_eq!(response.status, 429);
    assert_eq!(bytes(response).await, error);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(!requests[0].headers.contains_key("x-api-key"));
    let sent: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_searchless(&sent);
    assert!(store.saved.lock().unwrap().is_empty());
}

#[tokio::test]
async fn response_restoration_is_per_request_and_does_not_rename_a_later_native_alias() {
    let answer = serde_json::to_vec_pretty(&tool_answer()).unwrap();
    let (plugin, _, _) = configured(vec![
        response(&answer, "application/json", 7),
        response(&answer, "application/json", 7),
    ]);
    let mut body = claude_code();
    assert_eq!(
        decode(
            plugin
                .fetch(fetch_request(credentials("coding.key", ""), &body))
                .await
                .unwrap()
        )
        .await,
        restored_answer()
    );
    body["tools"]
        .as_array_mut()
        .unwrap()
        .retain(|tool| tool["name"] != "ToolSearch");
    assert_eq!(
        bytes(
            plugin
                .fetch(fetch_request(credentials("coding.key", ""), &body))
                .await
                .unwrap()
        )
        .await,
        answer
    );
}

#[tokio::test]
async fn undeclared_wrong_case_or_escaped_names_and_invalid_request_shapes_stay_byte_identical() {
    let answer = serde_json::to_vec(&tool_answer()).unwrap();
    for original in [
        br#"not JSON "ToolSearch""#.as_slice(),
        br#"[{"tools":[{"name":"ToolSearch"}]}]"#,
        br#"{"tools":{"name":"ToolSearch"}}"#,
        br#"{"messages":[],"metadata":{"user_id":"caller"},"tools":[{"name":"toolsearch"},{"name":"DeferredToolPlaceholder","defer_loading":true}],"literal":"ToolSearch"}"#,
        br#"{"messages":[],"metadata":{"user_id":"caller"},"tools":[{"name":"\u0054oolSearch"}]}"#,
    ] {
        let (plugin, client, _) = configured(vec![response(&answer,"application/json",7)]);
        let mut req = fetch_request(credentials("coding.key",""),&claude_code());
        req.body = Some(base64::engine::general_purpose::STANDARD.encode(original));
        assert_eq!(bytes(plugin.fetch(req).await.unwrap()).await,answer);
        assert_eq!(client.requests.lock().unwrap()[0].body.as_ref().unwrap(),original);
    }
}

#[tokio::test]
async fn malformed_tool_entries_keep_the_official_object_spread_behavior_without_adding_message_fields(
) {
    let original = json!({"metadata":{"user_id":"caller"},"messages":[{"role":"assistant"},null,false,{"role":"assistant","content":null}],"tools":[null,7,"literal",["entry"],{"name":"ToolSearch","defer_loading":false},{"name":"ToolSearch2","defer_loading":"true"}]});
    let (plugin, client, _) = configured(vec![response(b"{}", "application/json", 1)]);
    plugin
        .fetch(fetch_request(credentials("coding.key", ""), &original))
        .await
        .unwrap();
    let sent: Value =
        serde_json::from_slice(client.requests.lock().unwrap()[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(sent["messages"], original["messages"]);
    assert_eq!(
        sent["tools"],
        json!([null,7,"literal",{"0":"entry"},{"name":"ToolSearchTool"},{"name":"ToolSearch2"}])
    );
}

#[tokio::test]
async fn token_count_tool_conversion_does_not_add_a_device_marker() {
    let mut original = claude_code();
    original.as_object_mut().unwrap().remove("metadata");
    let (plugin, client, _) = configured(vec![response(
        b"{\"input_tokens\":42}",
        "application/json",
        3,
    )]);
    let mut req = fetch_request(credentials("coding.key", ""), &original);
    req.url = format!("{}/v1/messages/count_tokens", Site::Zai.base());
    assert_eq!(
        decode(plugin.fetch(req).await.unwrap()).await,
        json!({"input_tokens":42})
    );
    let sent: Value =
        serde_json::from_slice(client.requests.lock().unwrap()[0].body.as_ref().unwrap()).unwrap();
    assert_searchless(&sent);
    assert!(sent.get("metadata").is_none());
}

#[tokio::test]
async fn no_search_keeps_sse_chunk_bytes_and_a_native_alias_unchanged() {
    let original = sse(&events());
    let mut body = claude_code();
    body["tools"][3]["name"] = json!("ToolSearchTool");
    let (plugin, _, _) = configured(vec![response(&original, "text/event-stream", 7)]);
    let response = plugin
        .fetch(fetch_request(credentials("coding.key", ""), &body))
        .await
        .unwrap();
    let chunks = response
        .body
        .collect::<Vec<_>>()
        .await
        .into_iter()
        .collect::<PluginResult<Vec<_>>>()
        .unwrap();
    assert_eq!(
        chunks,
        original.chunks(7).map(<[u8]>::to_vec).collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn unexpected_json_response_shapes_are_preserved_without_panicking() {
    for original in [
        br#""ToolSearchTool""#.as_slice(),
        br#"["ToolSearchTool"]"#,
        br#"{"content":{"name":"ToolSearchTool"}}"#,
        br#"{"content":[null,42,"ToolSearchTool",[],{"type":"text","name":"ToolSearchTool"}]}"#,
        br#"{"content":[{"type":"tool_use","name":"\u0054oolSearchTool"}]}"#,
        b"",
    ] {
        let (plugin, _, _) = configured(vec![response(original, "application/json", 3)]);
        assert_eq!(
            bytes(
                plugin
                    .fetch(fetch_request(credentials("coding.key", ""), &claude_code()))
                    .await
                    .unwrap()
            )
            .await,
            original
        );
    }
}

#[tokio::test]
async fn transformed_responses_bound_json_and_unterminated_line_buffers() {
    for content_type in ["application/json", "text/event-stream"] {
        let over_limit = vec![b'x'; core::MAX_JSON + 1];
        let (plugin, _, _) = configured(vec![response(&over_limit, content_type, 1024 * 1024)]);
        let response = plugin
            .fetch(fetch_request(credentials("coding.key", ""), &claude_code()))
            .await
            .unwrap();
        let chunks = response.body.collect::<Vec<_>>().await;
        assert_eq!(chunks.len(), 1);
        assert!(chunks[0]
            .as_ref()
            .unwrap_err()
            .message
            .contains("size limit"));
    }
}
