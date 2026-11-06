use super::*;

fn identified(mut credentials: Value, subject: &str, scope: &str) -> Value {
    credentials["accountIdentity"] =
        json!({"issuer":"https://claude.ai","subject":subject,"scope":scope});
    credentials
}
#[tokio::test]
async fn separate_compiled_hosts_share_observed_allowance_without_running_usage_or_auth_again() {
    let directory = fixture::Directory::new();
    let account = directory.0.join("claude-accounts/account-a");
    let credentials = identified(fixture::profile(&account), "account-a", "org-a");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(account.join("fixture-rate-limit-event.json"),json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"seven_day_opus","utilization":0.9,"resetsAt":now+604800,"unifiedWindows":{"five_hour":{"utilization":0.42,"resetsAt":now+18000},"seven_day":{"utilization":0.17,"resetsAt":now+604800}}}}).to_string()).unwrap();
    std::fs::write(
        account.join("fixture-usage.json"),
        json!({"exit":1,"stdout":"too_many_requests: this command must never run"}).to_string(),
    )
    .unwrap();
    let mut a = Host::new(&directory.0).await;
    let id = a
        .fetch(
            credentials.clone(),
            "learned-account",
            "turn-1",
            vec![user("LEARN_LIMITS")],
            json!([]),
            false,
        )
        .await;
    let head = a.frame(id).await;
    assert_eq!(head["event"], "head");
    assert_eq!(
        head["headers"]["anthropic-ratelimit-unified-5h-utilization"],
        "0.42"
    );
    assert_eq!(
        head["headers"]["anthropic-ratelimit-unified-7d-reset"],
        (now + 604800).to_string()
    );
    a.queued.entry(id).or_default().push_back(head);
    let (status, body) = a.response(id).await;
    assert_eq!(status, 200);
    assert!(body.contains("content"));
    let observed_logs = logs(&account).len();
    let mut b = Host::new(&directory.0).await;
    let mut readonly = credentials.clone();
    readonly["extras"]["noraClaudeCLI"]["executable"] = json!("/this-program-must-never-run");
    let id = b
        .send(
            "learnedUsage",
            json!({"account":"other-slot","credentials":readonly}),
        )
        .await;
    let frame = b.frame(id).await;
    assert!(frame.get("error").is_none(), "{frame}");
    let value = &frame["result"];
    assert_eq!(value["account"]["id"], "other-slot");
    let items = value["items"].as_array().unwrap();
    assert_eq!(items.len(), 3);
    assert_eq!(
        items.iter().find(|item| item["id"] == "five_hour").unwrap()["usedPercent"],
        42.0
    );
    assert_eq!(
        items
            .iter()
            .find(|item| item["id"] == "seven_day_opus")
            .unwrap()["modelIDs"],
        json!(["*opus*"])
    );
    assert!(value["readAt"].as_i64().unwrap() > 0);
    assert!(chrono::DateTime::parse_from_rfc3339(value["observedAt"].as_str().unwrap()).is_ok());
    let id = b
        .send("usage", json!({"account":"work","credentials":readonly}))
        .await;
    assert!(b.frame(id).await.get("error").is_none());
    assert_eq!(logs(&account).len(), observed_logs);
    for other in [
        identified(credentials.clone(), "different-account", "org-a"),
        identified(credentials.clone(), "account-a", "different-org"),
        {
            let mut api = credentials.clone();
            api["type"] = json!("apiKey");
            api
        },
    ] {
        let id = b
            .send(
                "learnedUsage",
                json!({"account":"work","credentials":other}),
            )
            .await;
        assert_eq!(b.frame(id).await["result"], Value::Null);
    }
    let id = b
        .send(
            "learnedUsage",
            json!({"subscriptionAccountsEnabled":false,"account":"work","credentials":credentials}),
        )
        .await;
    let frame = b.frame(id).await;
    assert_eq!(frame["result"], Value::Null, "{frame}");
    assert_eq!(logs(&account).len(), observed_logs);
}

#[tokio::test]
async fn late_events_are_saved_by_the_stdout_reader_after_the_response_finishes() {
    let directory = fixture::Directory::new();
    let account = directory.0.join("claude-accounts/account-a");
    let credentials = identified(fixture::profile(&account), "account-a", "org-a");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    std::fs::write(account.join("fixture-late-rate-limit-event.json"),json!({"type":"rate_limit_event","rate_limit_info":{"status":"allowed","rateLimitType":"five_hour","utilization":0.63,"resetsAt":now+18000}}).to_string()).unwrap();
    let mut a = Host::new(&directory.0).await;
    let id = a
        .fetch(
            credentials.clone(),
            "late-cache",
            "turn-1",
            vec![user("LATE_LEARN_LIMITS")],
            json!([]),
            false,
        )
        .await;
    assert_eq!(a.response(id).await.0, 200);
    let mut b = Host::new(&directory.0).await;
    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(3);
    loop {
        let id = b
            .send("learnedUsage", json!({"credentials":credentials}))
            .await;
        let frame = b.frame(id).await;
        if frame["result"].is_object() {
            assert_eq!(frame["result"]["items"][0]["usedPercent"], 63.0);
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "late allowance event was not persisted"
        );
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
}
