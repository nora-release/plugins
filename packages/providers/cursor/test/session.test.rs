use super::*;
use crate::models::Selection;
use crate::{fixture, network, stream};
use async_trait::async_trait;
use nora_ai::{HttpClient, HttpClientError, HttpRequest, HttpStreamingResponse};
use std::sync::Arc;
use std::sync::Mutex as StdMutex;
use tokio::sync::mpsc;
#[derive(Clone, Copy)]
enum Mode {
    Tools,
    Truncated,
    Foreign,
    Conflict,
    RefuseResult,
}
struct Fake {
    mode: Mode,
    requests: StdMutex<Vec<HttpRequest>>,
    pipes: StdMutex<BTreeMap<String, mpsc::UnboundedSender<Vec<u8>>>>,
    submitted: StdMutex<BTreeMap<String, usize>>,
    results: StdMutex<Vec<(String, u64, Vec<u8>)>>,
    turn_usage: StdMutex<Option<Pb>>,
    run_starts: StdMutex<Vec<(String, u64)>>,
    usage_events: StdMutex<Value>,
    finish_bootstraps: std::sync::atomic::AtomicBool,
}
impl Fake {
    fn new(mode: Mode) -> Arc<Self> {
        Arc::new(Self {
            mode,
            requests: StdMutex::new(Vec::new()),
            pipes: StdMutex::new(BTreeMap::new()),
            submitted: StdMutex::new(BTreeMap::new()),
            results: StdMutex::new(Vec::new()),
            turn_usage: StdMutex::new(None),
            run_starts: StdMutex::new(vec![]),
            usage_events: StdMutex::new(json!([])),
            finish_bootstraps: std::sync::atomic::AtomicBool::new(false),
        })
    }
    fn initial(&self) -> Vec<u8> {
        if self
            .finish_bootstraps
            .load(std::sync::atomic::Ordering::SeqCst)
        {
            let mut out = fixture::update(1, Pb::new().str(1, "Imported history completed."));
            out.extend(fixture::update(14, Pb::new().num(1, 120).num(2, 30)));
            return out;
        }
        if matches!(self.mode, Mode::Truncated) {
            return vec![0, 0, 0, 0, 9, 10, 1];
        }
        let mut out = fixture::update(1, Pb::new().str(1, "Checking."));
        out.extend(fixture::update(4, Pb::new().str(1, "private reasoning")));
        if matches!(self.mode, Mode::Foreign) {
            let args = Pb::new()
                .str(1, "other-echo")
                .str(3, "native-foreign")
                .str(4, "other")
                .str(5, "echo");
            out.extend(protobuf::frame(
                &Pb::new().bytes(2, Pb::new().num(1, 99).bytes(11, args)).0,
            ));
            out.extend(fixture::update(27, Pb::new().num(1, 1)));
            out.extend(fixture::update(
                1,
                Pb::new().str(1, "Refused foreign tool."),
            ));
            out.extend(fixture::update(14, Pb::new().num(1, 12).num(2, 8)));
            return out;
        }
        for id in [10, 11] {
            let args = Pb::new()
                .str(1, "magpie-echo")
                .str(3, &format!("native-{id}"))
                .str(4, "magpie")
                .str(5, "echo")
                .bytes(
                    2,
                    Pb::new()
                        .str(1, "value")
                        .bytes(2, protobuf::value(&json!(id))),
                );
            let call = Pb::new().bytes(
                2,
                Pb::new()
                    .num(1, id)
                    .str(15, &format!("exec-{id}"))
                    .bytes(11, args),
            );
            out.extend(protobuf::frame(&call.0));
            if id == 10 {
                out.extend(protobuf::frame(&call.0));
            }
        }
        out.extend(fixture::update(27, Pb::new().num(1, 2)));
        if matches!(self.mode, Mode::Conflict) {
            out.extend(fixture::update(27, Pb::new().num(1, 3)));
        }
        out
    }
    fn open_count(&self) -> usize {
        self.requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.url.ends_with("/RunSSE"))
            .count()
    }
    fn ids(&self) -> Vec<String> {
        self.pipes.lock().unwrap().keys().cloned().collect()
    }
    fn closed(&self, id: &str) -> bool {
        self.pipes.lock().unwrap()[id].is_closed()
    }
}
#[async_trait]
impl HttpClient for Fake {
    async fn stream(&self, request: HttpRequest) -> Result<HttpStreamingResponse, HttpClientError> {
        self.requests.lock().unwrap().push(request.clone());
        if request.url.ends_with("/GetFilteredUsageEvents") {
            let bytes = json!({"usageEventsDisplay":self.usage_events.lock().unwrap().clone()})
                .to_string()
                .into_bytes();
            return Ok(HttpStreamingResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: Box::pin(futures::stream::once(async move { Ok(bytes.into()) })),
            });
        }
        let body = request.body.as_ref().unwrap();
        if request.url.ends_with("/RunSSE") {
            assert_eq!(
                request.headers["x-cursor-agent-allowed-tools"],
                "mcp_tool_call,get_mcp_tools_tool_call"
            );
            assert_eq!(request.headers["x-ghost-mode"], "true");
            let data = &body[5..];
            let id = protobuf::string(&protobuf::fields(data).unwrap(), 1).unwrap();
            let (tx, rx) = mpsc::unbounded_channel::<Vec<u8>>();
            self.pipes.lock().unwrap().insert(id, tx);
            return Ok(HttpStreamingResponse {
                status: 200,
                headers: BTreeMap::new(),
                body: Box::pin(futures::stream::unfold(rx, |mut rx| async move {
                    rx.recv().await.map(|b| (Ok(b.into()), rx))
                })),
            });
        }
        assert!(request.url.ends_with("/BidiAppend"));
        let append = protobuf::fields(body).unwrap();
        let id = protobuf::string(
            &protobuf::fields(protobuf::field(&append, 2).unwrap().data).unwrap(),
            1,
        )
        .unwrap();
        let hex = protobuf::string(&append, 1).unwrap();
        let bytes = (0..hex.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&hex[i..i + 2], 16).unwrap())
            .collect::<Vec<_>>();
        let message = protobuf::fields(&bytes).unwrap();
        if let Some(first) = protobuf::field(&message, 1) {
            let cid = protobuf::string(&protobuf::fields(first.data).unwrap(), 5).unwrap();
            self.run_starts.lock().unwrap().push((cid, auth::now()));
            self.pipes.lock().unwrap()[&id]
                .send(self.initial())
                .unwrap();
            if matches!(self.mode, Mode::Truncated) {
                self.pipes.lock().unwrap().remove(&id);
            }
        }
        if let Some(exec) = protobuf::field(&message, 2) {
            let fs = protobuf::fields(exec.data).unwrap();
            if let Some(result) = protobuf::field(&fs, 11) {
                let request_id = protobuf::num(&fs, 1).unwrap();
                assert_eq!(
                    protobuf::string(&fs, 15).unwrap(),
                    format!("exec-{request_id}")
                );
                self.results
                    .lock()
                    .unwrap()
                    .push((id.clone(), request_id, result.data.to_vec()));
                if matches!(self.mode, Mode::RefuseResult) {
                    return Ok(HttpStreamingResponse {
                        status: 403,
                        headers: BTreeMap::new(),
                        body: Box::pin(futures::stream::once(async {
                            Ok(br#"{"code":"permission_denied","message":"region moved"}"#
                                .to_vec()
                                .into())
                        })),
                    });
                }
                let mut submitted = self.submitted.lock().unwrap();
                let n = submitted.entry(id.clone()).or_default();
                *n += 1;
                if *n == 2 {
                    let mut answer = fixture::update(1, Pb::new().str(1, "Both results received."));
                    let usage = self.turn_usage.lock().unwrap().take().unwrap_or_else(|| {
                        Pb::new()
                            .num(1, 120)
                            .num(2, 30)
                            .num(3, 40)
                            .num(4, 10)
                            .num(5, 5)
                    });
                    answer.extend(fixture::update(14, usage));
                    self.pipes.lock().unwrap()[&id].send(answer).unwrap();
                }
            }
        }
        Ok(HttpStreamingResponse {
            status: 200,
            headers: BTreeMap::new(),
            body: Box::pin(futures::stream::once(async { Ok(b"{}".to_vec().into()) })),
        })
    }
}
fn config(client: Arc<Fake>, chat: &Value, account: &str) -> Open {
    Open {
        team_id: None,
        client,
        h2_client: None,
        token: format!("private-{account}"),
        version: network::FALLBACK_VERSION.into(),
        base: "https://agentn.global.api5.cursor.sh".into(),
        account: account.into(),
        controls: format!("{account}-same-tools-model-and-proxy"),
        selector: auth::hash(format!("{account}-session")),
        selection: Selection {
            id: "claude-medium".into(),
            params: BTreeMap::new(),
            max_mode: false,
        },
        tools: request::tools(chat).unwrap(),
        network_scope: "fixture-network".into(),
    }
}
async fn round(pool: &Pool, client: Arc<Fake>, chat: &Value, account: &str) -> Value {
    let run = pool
        .acquire(config(client, chat, account), chat)
        .await
        .unwrap();
    let body = fixture::body(stream::response(run, chat).await.unwrap()).await;
    serde_json::from_str(&body).unwrap()
}
fn answered(mut chat: Value, reply: &Value) -> Value {
    let message = reply["choices"][0]["message"].clone();
    let calls = message["tool_calls"].as_array().unwrap().clone();
    chat["messages"].as_array_mut().unwrap().push(message);
    for call in calls.into_iter().rev() {
        chat["messages"].as_array_mut().unwrap().push(json!({"role":"tool","tool_call_id":call["id"],"content":format!("result for {}",call["id"])}));
    }
    chat
}
#[tokio::test]
async fn real_connect_wire_keeps_parallel_results_on_one_run_and_counts_usage_once() {
    let client = Fake::new(Mode::Tools);
    let pool = Pool::default();
    let mut chat = fixture::chat();
    chat["stream"] = json!(false);
    let first = round(&pool, client.clone(), &chat, "one").await;
    assert_eq!(first["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(
        first["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap()
            .len(),
        2
    );
    assert_eq!(first["usage"]["total_tokens"], 0);
    let chat = answered(chat, &first);
    let final_reply = round(&pool, client.clone(), &chat, "one").await;
    assert_eq!(
        final_reply["choices"][0]["message"]["content"],
        "Both results received."
    );
    let usage = &final_reply["usage"];
    let earlier = &usage["magpie_deferred"];
    assert_eq!(
        usage["prompt_tokens"].as_u64().unwrap() + earlier["prompt_tokens"].as_u64().unwrap_or(0),
        120
    );
    assert_eq!(
        usage["completion_tokens"].as_u64().unwrap()
            + earlier["completion_tokens"].as_u64().unwrap_or(0),
        30
    );
    assert_eq!(
        usage["prompt_tokens_details"]["cached_tokens"]
            .as_u64()
            .unwrap()
            + earlier["cached_tokens"].as_u64().unwrap_or(0),
        40
    );
    assert_eq!(client.open_count(), 1);
    assert_eq!(client.results.lock().unwrap().len(), 2);
    let requests = client.requests.lock().unwrap();
    let sequences: Vec<_> = requests
        .iter()
        .filter(|r| r.url.ends_with("/BidiAppend"))
        .map(|r| protobuf::num(&protobuf::fields(r.body.as_ref().unwrap()).unwrap(), 3).unwrap())
        .collect();
    assert_eq!(sequences, (0..sequences.len() as u64).collect::<Vec<_>>());
}

#[tokio::test]
async fn cumulative_native_usage_keeps_earlier_steps_out_of_the_final_context_size() {
    let client = Fake::new(Mode::Tools);
    *client.turn_usage.lock().unwrap() = Some(
        Pb::new()
            .num(1, 1_105_934)
            .num(2, 2_100)
            .num(3, 1_035_734)
            .num(4, 70_156)
            .num(5, 900),
    );
    let pool = Pool::default();
    let mut chat = fixture::chat();
    chat["stream"] = json!(false);
    let first = round(&pool, client.clone(), &chat, "one").await;
    assert_eq!(first["usage"]["total_tokens"], 0);
    let reply = round(&pool, client.clone(), &answered(chat, &first), "one").await;
    let usage = &reply["usage"];
    let earlier = &usage["magpie_deferred"];
    assert!(usage["prompt_tokens"].as_u64().unwrap() < 1_105_934);
    assert!(usage["prompt_tokens"].as_u64().unwrap() > 0);
    for (name, expected) in [("prompt_tokens", 1_105_934), ("completion_tokens", 2_100)] {
        assert_eq!(
            usage[name].as_u64().unwrap() + earlier[name].as_u64().unwrap(),
            expected
        );
    }
    for (name, expected) in [("cached_tokens", 1_035_734), ("cache_write_tokens", 70_156)] {
        assert_eq!(
            usage["prompt_tokens_details"][name].as_u64().unwrap()
                + earlier[name].as_u64().unwrap(),
            expected
        );
    }
    assert!(
        usage["completion_tokens_details"]["reasoning_tokens"]
            .as_u64()
            .unwrap()
            <= usage["completion_tokens"].as_u64().unwrap()
    );
    assert_eq!(client.open_count(), 1);
}

#[tokio::test]
async fn explicit_zero_native_usage_remains_authoritative_after_tools() {
    let client = Fake::new(Mode::Tools);
    *client.turn_usage.lock().unwrap() = Some(Pb::new().num(1, 0).num(2, 0).num(3, 0).num(4, 0));
    let pool = Pool::default();
    let mut chat = fixture::chat();
    chat["stream"] = json!(false);
    let first = round(&pool, client.clone(), &chat, "one").await;
    let reply = round(&pool, client, &answered(chat, &first), "one").await;
    assert_eq!(reply["usage"]["total_tokens"], 0);
    assert_eq!(reply["usage"]["cursor_accounting"], "measured");
    assert!(reply["usage"]["magpie_deferred"].is_null());
}

#[test]
fn native_cache_only_usage_is_measured_instead_of_replaced_by_a_prompt_guess() {
    let wire = Pb::new().num(2, 3).num(3, 800).num(4, 20);
    let usage = split_usage(&protobuf::fields(&wire.0).unwrap(), 4_000, 100, 1, 100);
    assert_eq!(usage["prompt_tokens"], 820);
    assert_eq!(usage["cursor_accounting"], "measured");
}

#[tokio::test]
async fn cancelling_a_pending_native_run_recovers_its_late_usage_on_the_next_completed_run_once() {
    let client = Fake::new(Mode::Tools);
    let pool = Pool::default();
    let mut chat = fixture::chat();
    chat["stream"] = json!(false);
    abandoned_usage(&pool, client.clone(), &chat).await;
    let first = round(&pool, client.clone(), &chat, "one").await;
    let final_reply = round(
        &pool,
        client.clone(),
        &answered(chat.clone(), &first),
        "one",
    )
    .await;
    let sum = |reply: &Value, name: &str| {
        reply["usage"][name].as_u64().unwrap()
            + reply["usage"]["magpie_deferred"][name]
                .as_u64()
                .unwrap_or(0)
    };
    assert_eq!(sum(&final_reply, "prompt_tokens"), 120 + 118);
    assert_eq!(sum(&final_reply, "completion_tokens"), 30 + 3);
    let later = round(&pool, client.clone(), &chat, "one").await;
    let later = round(&pool, client.clone(), &answered(chat, &later), "one").await;
    assert_eq!(sum(&later, "prompt_tokens"), 120);
    assert_eq!(sum(&later, "completion_tokens"), 30);
    assert_eq!(
        client
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|request| request.url.ends_with("/GetFilteredUsageEvents"))
            .count(),
        1
    );
}

async fn abandoned_usage(pool: &Pool, client: Arc<Fake>, chat: &Value) {
    let mut cancelled = pool
        .acquire(config(client.clone(), &chat, "one"), &chat)
        .await
        .unwrap();
    loop {
        if matches!(
            cancelled.run.next().await.unwrap(),
            Some(Event::Finish("tool_calls", _))
        ) {
            break;
        }
    }
    drop(cancelled);
    let (cid, at) = client.run_starts.lock().unwrap()[0].clone();
    *client.usage_events.lock().unwrap() = json!([{"timestamp":at.to_string(),"conversationId":cid,"tokenUsage":{"inputTokens":11,"outputTokens":3,"cacheReadTokens":100,"cacheWriteTokens":7}}]);
    while auth::now() <= at {
        tokio::time::sleep(Duration::from_millis(1)).await;
    }
}

#[tokio::test]
async fn stopping_after_the_usage_chunk_or_done_never_reports_delivered_deferred_usage_again() {
    use futures::StreamExt;
    for stop_at_done in [false, true] {
        let client = Fake::new(Mode::Tools);
        let pool = Pool::default();
        let mut chat = fixture::chat();
        chat["stream"] = json!(false);
        abandoned_usage(&pool, client.clone(), &chat).await;
        let first = round(&pool, client.clone(), &chat, "one").await;
        let mut next = answered(chat.clone(), &first);
        next["stream"] = json!(true);
        let run = pool
            .acquire(config(client.clone(), &next, "one"), &next)
            .await
            .unwrap();
        let mut response = stream::response(run, &next).await.unwrap();
        loop {
            let chunk = String::from_utf8(response.body.next().await.unwrap().unwrap()).unwrap();
            if chunk.contains(if stop_at_done { "[DONE]" } else { "\"usage\"" }) {
                break;
            }
        }
        drop(response);
        let later = round(&pool, client.clone(), &chat, "one").await;
        let later = round(&pool, client.clone(), &answered(chat, &later), "one").await;
        let usage = &later["usage"];
        assert_eq!(
            usage["prompt_tokens"].as_u64().unwrap()
                + usage["magpie_deferred"]["prompt_tokens"]
                    .as_u64()
                    .unwrap_or(0),
            120
        );
        assert_eq!(
            client
                .requests
                .lock()
                .unwrap()
                .iter()
                .filter(|request| request.url.ends_with("/GetFilteredUsageEvents"))
                .count(),
            1
        );
    }
}

#[tokio::test]
async fn a_tool_response_closed_after_done_still_resumes_its_original_run() {
    use futures::StreamExt;
    let client = Fake::new(Mode::Tools);
    let pool = Pool::default();
    let mut chat = fixture::chat();
    chat["stream"] = json!(false);
    let run = pool
        .acquire(config(client.clone(), &chat, "one"), &chat)
        .await
        .unwrap();
    let mut streaming = chat.clone();
    streaming["stream"] = json!(true);
    let mut response = stream::response(run, &streaming).await.unwrap();
    let mut text = String::new();
    let mut reasoning = String::new();
    let mut calls = vec![];
    loop {
        let chunk = String::from_utf8(response.body.next().await.unwrap().unwrap()).unwrap();
        if chunk.contains("[DONE]") {
            break;
        }
        let value: Value =
            serde_json::from_str(chunk.trim().strip_prefix("data: ").unwrap()).unwrap();
        let delta = &value["choices"][0]["delta"];
        text.push_str(delta["content"].as_str().unwrap_or(""));
        reasoning.push_str(delta["reasoning_content"].as_str().unwrap_or(""));
        if let Some(items) = delta["tool_calls"].as_array() {
            calls.extend(items.clone());
        }
    }
    drop(response);
    let first = json!({"choices":[{"message":{"role":"assistant","content":text,"reasoning_content":reasoning,"tool_calls":calls}}]});
    let final_reply = round(&pool, client.clone(), &answered(chat, &first), "one").await;
    assert_eq!(final_reply["choices"][0]["finish_reason"], "stop");
    assert_eq!(client.open_count(), 1);
}

#[tokio::test]
async fn retention_capacity_rebuilds_full_history_without_closing_or_reusing_active_parents() {
    for per_account in [true, false] {
        let client = Fake::new(Mode::Tools);
        let pool = Pool::default();
        let limit = if per_account { 8 } else { 32 };
        for index in 0..limit {
            let mut chat = fixture::chat();
            chat["stream"] = json!(false);
            chat["messages"][1]["content"] = json!(format!("Parent {index}"));
            let account = if per_account {
                "one".to_owned()
            } else {
                format!("parent-{index}")
            };
            let reply = round(&pool, client.clone(), &chat, &account).await;
            assert_eq!(reply["choices"][0]["finish_reason"], "tool_calls");
        }
        let parents: BTreeSet<_> = client.ids().into_iter().collect();
        let account = if per_account { "one" } else { "overflow" };
        let mut chat = fixture::chat();
        chat["stream"] = json!(false);
        chat["messages"][1]["content"] = json!("The capacity fallback still needs all my history.");
        let first = round(&pool, client.clone(), &chat, account).await;
        assert_eq!(first["choices"][0]["finish_reason"], "tool_calls");
        let overflow = client
            .ids()
            .into_iter()
            .find(|id| !parents.contains(id))
            .unwrap();
        assert!(client.closed(&overflow));
        assert!(parents.iter().all(|id| !client.closed(id)));
        let (cid, at) = client.run_starts.lock().unwrap().last().cloned().unwrap();
        *client.usage_events.lock().unwrap() = json!([{"timestamp":at,"conversationId":cid,"tokenUsage":{"inputTokens":11,"outputTokens":3}}]);
        while auth::now() <= at {
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
        client
            .finish_bootstraps
            .store(true, std::sync::atomic::Ordering::SeqCst);
        let next = answered(chat, &first);
        let run = pool
            .acquire(config(client.clone(), &next, account), &next)
            .await
            .unwrap();
        assert!(!run.run.retained);
        assert_eq!(
            run.run.history,
            next["messages"].as_array().unwrap().clone()
        );
        assert!(run.run.blobs.values().any(|bytes| bytes
            .windows(b"result for ".len())
            .any(|part| part == b"result for ")));
        let response = stream::response(run, &next).await.unwrap();
        let reply: Value = serde_json::from_str(&fixture::body(response).await).unwrap();
        assert_eq!(
            reply["choices"][0]["message"]["content"],
            "Imported history completed."
        );
        assert_eq!(reply["usage"]["magpie_deferred"]["prompt_tokens"], 11);
        assert!(client.results.lock().unwrap().is_empty());
        assert!(parents.iter().all(|id| !client.closed(id)));
        assert_eq!(client.open_count(), limit + 2);
    }
}

#[tokio::test]
async fn a_region_refusal_during_tool_result_upload_cannot_trigger_another_run() {
    let client = Fake::new(Mode::RefuseResult);
    let pool = Pool::default();
    let mut chat = fixture::chat();
    chat["stream"] = json!(false);
    let first = round(&pool, client.clone(), &chat, "one").await;
    let next = answered(chat, &first);
    let error = pool
        .acquire(config(client.clone(), &next, "one"), &next)
        .await
        .err()
        .unwrap();
    assert_eq!(error.code, "upstream_transport_error");
    assert_eq!(client.results.lock().unwrap().len(), 1);
    assert_eq!(client.open_count(), 1);
}
#[tokio::test]
async fn cancellation_closes_only_its_own_stream_and_another_account_continues() {
    let client = Fake::new(Mode::Tools);
    let pool = Pool::default();
    let mut chat = fixture::chat();
    chat["stream"] = json!(false);
    let other = round(&pool, client.clone(), &chat, "other").await;
    let other_id = client.ids()[0].clone();
    let mut streaming = chat.clone();
    streaming["stream"] = json!(true);
    let run = pool
        .acquire(config(client.clone(), &streaming, "cancelled"), &streaming)
        .await
        .unwrap();
    let response = stream::response(run, &streaming).await.unwrap();
    let cancelled_id = client.ids().into_iter().find(|id| id != &other_id).unwrap();
    drop(response);
    assert!(client.closed(&cancelled_id));
    assert!(!client.closed(&other_id));
    let final_reply = round(&pool, client.clone(), &answered(chat, &other), "other").await;
    assert_eq!(final_reply["choices"][0]["finish_reason"], "stop");
    assert_eq!(client.open_count(), 2);
}
#[tokio::test]
async fn changed_history_or_account_never_submits_results_to_a_retained_exec() {
    let client = Fake::new(Mode::Tools);
    let pool = Pool::default();
    let mut chat = fixture::chat();
    chat["stream"] = json!(false);
    let first = round(&pool, client.clone(), &chat, "one").await;
    let chat = answered(chat, &first);
    let other = round(&pool, client.clone(), &chat, "different-account").await;
    assert_eq!(other["choices"][0]["finish_reason"], "tool_calls");
    assert_eq!(client.open_count(), 2);
    assert!(client.results.lock().unwrap().is_empty());
    let mut changed = chat;
    changed["messages"][0]["content"] = json!("Changed system instructions");
    let _ = round(&pool, client.clone(), &changed, "one").await;
    assert_eq!(client.open_count(), 3);
    assert!(client.results.lock().unwrap().is_empty());
}
#[tokio::test]
async fn foreign_mcp_and_builtin_execution_are_refused_without_publishing_a_tool_call() {
    let client = Fake::new(Mode::Foreign);
    let pool = Pool::default();
    let mut chat = fixture::chat();
    chat["stream"] = json!(false);
    let reply = round(&pool, client.clone(), &chat, "one").await;
    assert!(reply["choices"][0]["message"]["tool_calls"].is_null());
    let requests = client.requests.lock().unwrap();
    assert!(
        requests.iter().filter_map(|r| r.body.as_ref()).any(|b| b
            .windows("not available".len())
            .any(|s| s == b"not available"))
            == false,
        "Bidi payload is hex encoded"
    );
    let refused = requests
        .iter()
        .filter(|r| r.url.ends_with("/BidiAppend"))
        .any(|r| {
            let fs = protobuf::fields(r.body.as_ref().unwrap()).unwrap();
            protobuf::string(&fs, 1).unwrap().contains(
                &b"not available"
                    .iter()
                    .map(|b| format!("{b:02x}"))
                    .collect::<String>(),
            )
        });
    assert!(refused);
}
#[tokio::test]
async fn malformed_frames_and_conflicting_native_batch_counts_are_terminal() {
    for mode in [Mode::Truncated, Mode::Conflict] {
        let client = Fake::new(mode);
        let pool = Pool::default();
        let chat = fixture::chat();
        let run = pool
            .acquire(config(client.clone(), &chat, "one"), &chat)
            .await
            .unwrap();
        assert!(stream::response(run, &chat).await.is_err());
        assert!(client.results.lock().unwrap().is_empty());
    }
}
#[test]
fn assistant_normalization_keeps_reasoning_and_json_argument_identity() {
    assert_eq!(
        normalize(
            &json!({"role":"assistant","content":"x","reasoning":"r","tool_calls":[{"id":"a","function":{"name":"f","arguments":"{ \"x\":1 }"}}]})
        ),
        normalize(
            &json!({"role":"assistant","content":"x","reasoning_content":"r","tool_calls":[{"id":"a","function":{"name":"f","arguments":{"x":1}}}]})
        )
    );
}
