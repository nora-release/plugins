use super::*;
use crate::tests::{answer, collect, configured, credentials, Client};
use base64::Engine;

fn account(site: Site) -> Value {
    let mut value = credentials();
    if site.is_work() {
        value["deviceChat"] = json!(true);
    }
    if site == Site::QwenWorkEnterprise {
        value["enterpriseHost"] = json!("acme.example.com");
        value["vpc"] = json!("acme.example.com");
    }
    value
}
fn catalog(site: Site) -> Value {
    json!({site.scene():[{"key":"model","source":"source","enable":true,"max_input_tokens":131072}]})
}
fn queued() -> Value {
    json!({"code":"10605","message":json!({"isQueued":true,"modelKey":"model","queueType":"global","retryAfterSeconds":30}).to_string()})
}
fn request(provider: &Qoder, stream: bool) -> FetchRequest {
    let credentials = account(provider.site);
    serde_json::from_value(json!({"account":"one","credentials":credentials,"url":format!("{}/v1/chat/completions",provider.deployment(&credentials).unwrap().api),"method":"POST","metadata":{"sessionID":"private-nora-session"},"body":base64::engine::general_purpose::STANDARD.encode(json!({"model":"model","messages":[{"role":"user","content":"hi"}],"stream":stream}).to_string())})).unwrap()
}
fn chats(client: &Client) -> Vec<Value> {
    client
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.url.contains("agent_chat_generation"))
        .map(|r| {
            serde_json::from_str(
                &codec::decode(std::str::from_utf8(r.body.as_ref().unwrap()).unwrap()).unwrap(),
            )
            .unwrap()
        })
        .collect()
}
fn statuses(client: &Client) -> Vec<HttpRequest> {
    client
        .requests
        .lock()
        .unwrap()
        .iter()
        .filter(|r| r.url.contains("/queue/status"))
        .cloned()
        .collect()
}

#[tokio::test(start_paused = true)]
async fn public_and_enterprise_queue_waits_keep_the_same_request_and_sign_each_site_status() {
    for site in [Site::QwenWork, Site::QwenWorkEnterprise] {
        let client = Client::new(vec![
            (200, catalog(site)),
            (403, queued()),
            (
                200,
                json!({"data":{"isQueued":true,"modelKey":"queue-model","queueType":"changed","retryAfterSeconds":0.1}}),
            ),
            (
                200,
                json!({"message":json!({"isQueued":true,"retryAfterSeconds":300}).to_string()}),
            ),
            (200, json!({"isQueued":true})),
            (
                200,
                json!({"result":{"isQueued":false,"retryAfterSeconds":1}}),
            ),
            (200, answer()),
        ]);
        let provider = configured(site, client.clone());
        let started = Instant::now();
        let response = provider.fetch(request(&provider, false)).await.unwrap();
        assert_eq!(response.status, 200);
        assert!(String::from_utf8(collect(response).await)
            .unwrap()
            .contains("hello"));
        assert_eq!(started.elapsed(), Duration::from_millis(61_500));
        let asks = chats(&client);
        assert_eq!(asks.len(), 2);
        assert_eq!(asks[0], asks[1]);
        for field in [
            "request_set_id",
            "request_id",
            "chat_record_id",
            "session_id",
        ] {
            assert!(!text(&asks[0], field).is_empty());
        }
        assert_eq!(asks[0]["request_set_id"], asks[0]["business"]["id"]);
        assert_eq!(asks[0]["request_id"], asks[0]["chat_record_id"]);
        assert_ne!(asks[0]["session_id"], "private-nora-session");
        let statuses = statuses(&client);
        assert_eq!(statuses.len(), 4);
        for (index, status) in statuses.iter().enumerate() {
            let url = url::Url::parse(&status.url).unwrap();
            let query = url.query_pairs().collect::<BTreeMap<_, _>>();
            assert_eq!(
                url.origin().ascii_serialization(),
                provider.deployment(&account(site)).unwrap().api
            );
            assert_eq!(query["requestSetId"], text(&asks[0], "request_set_id"));
            assert_eq!(
                query["modelKey"],
                if index == 0 { "model" } else { "queue-model" }
            );
            assert_eq!(
                query["queueType"],
                if index == 0 { "global" } else { "changed" }
            );
            assert_eq!(status.method, HttpMethod::Get);
            assert!(status.body.is_none());
            assert!(!status.headers.contains_key("content-type"));
            assert_eq!(status.headers["x-request-id"], text(&asks[0], "request_id"));
            assert_eq!(status.headers["x-session-id"], text(&asks[0], "session_id"));
            assert!(status.headers["authorization"].starts_with("Bearer COSY."));
            assert_eq!(status.headers["cosy-clienttype"], "6");
            assert_eq!(status.headers["cosy-scene"], "qwork");
            assert_eq!(status.headers["cosy-version"], "1.1.59");
        }
    }
}

#[tokio::test(start_paused = true)]
async fn a_queue_refusal_in_the_first_split_stream_event_is_recovered_before_any_answer() {
    let site = Site::QwenWork;
    let stream = format!(
        "data: {}\n\n",
        json!({"statusCodeValue":403,"body":queued().to_string()})
    );
    let client = Client::new(vec![
        (200, catalog(site)),
        (200, json!({"raw":stream,"split":true})),
        (
            200,
            json!({"isQueued":false,"serviceAvailable":false,"retryAfterSeconds":0}),
        ),
        (200, answer()),
    ]);
    let provider = configured(site, client.clone());
    let response = provider.fetch(request(&provider, true)).await.unwrap();
    assert_eq!(response.status, 200);
    let body = String::from_utf8(collect(response).await).unwrap();
    assert!(body.contains("hello"));
    assert!(!body.contains("10605"));
    assert!(!body.contains("qoder_error"));
    assert!(body.contains("[DONE]"));
    assert_eq!(statuses(&client).len(), 1);
    assert_eq!(chats(&client).len(), 2);
}

#[tokio::test(start_paused = true)]
async fn absent_or_unreadable_queue_status_ends_waiting_without_touching_the_account() {
    for (status, body) in [
        (404, json!({})),
        (200, json!({"data":{"queueCount":1}})),
        (200, json!({"raw":"not-json"})),
        (200, json!({"serviceAvailable":true})),
    ] {
        let client = Client::new(vec![
            (200, catalog(Site::QwenWork)),
            (403, queued()),
            (status, body),
        ]);
        let provider = configured(Site::QwenWork, client.clone());
        let response = provider.fetch(request(&provider, false)).await.unwrap();
        assert_eq!(response.status, 503);
        assert_eq!(
            response.headers.get("retry-after").map(String::as_str),
            Some("30")
        );
        assert!(!response.headers.contains_key("x-magpie-sign-in"));
        assert_eq!(statuses(&client).len(), 1);
        assert_eq!(chats(&client).len(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn three_consecutive_status_failures_end_the_wait_and_a_good_status_resets_the_counter() {
    for (states, expected_status, expected_asks) in [
        (
            vec![(502, json!({})), (500, json!({})), (503, json!({}))],
            503,
            3,
        ),
        (
            vec![
                (502, json!({})),
                (200, json!({"isQueued":true,"retryAfterSeconds":1})),
                (502, json!({})),
                (502, json!({})),
                (200, json!({"isQueued":false})),
            ],
            200,
            5,
        ),
    ] {
        let mut replies = vec![(200, catalog(Site::QwenWork)), (403, queued())];
        replies.extend(states);
        if expected_status == 200 {
            replies.push((200, answer()));
        }
        let client = Client::new(replies);
        let provider = configured(Site::QwenWork, client.clone());
        assert_eq!(
            provider
                .fetch(request(&provider, false))
                .await
                .unwrap()
                .status,
            expected_status
        );
        assert_eq!(statuses(&client).len(), expected_asks);
    }
}

#[tokio::test(start_paused = true)]
async fn a_request_waits_at_most_four_minutes_and_is_recovered_at_most_ten_times() {
    let mut replies = vec![(200, catalog(Site::QwenWork)), (403, queued())];
    replies.extend((0..8).map(|_| (200, json!({"isQueued":true,"retryAfterSeconds":9999}))));
    let client = Client::new(replies);
    let provider = configured(Site::QwenWork, client.clone());
    let started = Instant::now();
    assert_eq!(
        provider
            .fetch(request(&provider, false))
            .await
            .unwrap()
            .status,
        503
    );
    assert_eq!(started.elapsed(), BUDGET);
    assert_eq!(statuses(&client).len(), 8);
    assert_eq!(chats(&client).len(), 1);
    let mut replies = vec![(200, catalog(Site::QwenWork))];
    for _ in 0..RECOVERIES {
        replies.extend([(403, queued()), (200, json!({"isQueued":false}))]);
    }
    replies.push((403, queued()));
    let client = Client::new(replies);
    let provider = configured(Site::QwenWork, client.clone());
    assert_eq!(
        provider
            .fetch(request(&provider, false))
            .await
            .unwrap()
            .status,
        503
    );
    assert_eq!(statuses(&client).len(), 10);
    assert_eq!(chats(&client).len(), 11);
}

#[tokio::test(start_paused = true)]
async fn real_authentication_quota_and_request_refusals_never_enter_the_queue() {
    for (body, status) in [
        (json!({"message":"denied"}), 401),
        (json!({"code":110,"message":"spent"}), 429),
        (json!({"code":406,"message":"invalid"}), 400),
    ] {
        let client = Client::new(vec![(200, catalog(Site::QwenWork)), (403, body)]);
        let provider = configured(Site::QwenWork, client.clone());
        assert_eq!(
            provider
                .fetch(request(&provider, false))
                .await
                .unwrap()
                .status,
            status
        );
        assert!(statuses(&client).is_empty());
    }
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_request_stops_its_status_polling_and_never_retries_the_chat() {
    let client = Client::new(vec![
        (200, catalog(Site::QwenWork)),
        (403, queued()),
        (200, json!({"isQueued":true,"retryAfterSeconds":30})),
    ]);
    let provider = Arc::new(configured(Site::QwenWork, client.clone()));
    let req = request(&provider, false);
    let task = tokio::spawn(async move { provider.fetch(req).await });
    while statuses(&client).is_empty() {
        tokio::task::yield_now().await;
    }
    task.abort();
    assert!(matches!(task.await,Err(error) if error.is_cancelled()));
    tokio::time::advance(Duration::from_secs(120)).await;
    assert_eq!(statuses(&client).len(), 1);
    assert_eq!(chats(&client).len(), 1);
}

#[test]
fn queue_status_unwraps_boolean_strings_and_keeps_a_finite_delay() {
    let value=state(&json!({"data":json!({"message":json!({"isQueued":"true","retryAfterSeconds":"7"}).to_string()}).to_string()})).unwrap();
    assert!(value.busy());
    assert_eq!(value.after, Some(7.0));
    assert!(state(&json!({"data":{"queueCount":2}})).is_none());
    assert_eq!(pause(Some(0.1)), Duration::from_millis(500));
    assert_eq!(pause(Some(300.0)), Duration::from_secs(30));
    assert_eq!(pause(None), Duration::from_secs(30));
    assert_eq!(pause(Some(f64::NAN)), Duration::from_secs(30));
}
