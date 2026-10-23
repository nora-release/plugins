use super::*;
use crate::{
    fixture::{Client, Reply},
    models::Selection,
};

fn config(client: Arc<Client>, account: &str) -> Open {
    Open {
        client,
        h2_client: None,
        token: format!("fixture-{account}"),
        version: "cli-fixture".into(),
        base: network::API.into(),
        account: account.into(),
        controls: "controls".into(),
        selector: "session".into(),
        selection: Selection {
            id: "fixture".into(),
            params: BTreeMap::new(),
            max_mode: false,
        },
        tools: vec![],
        network_scope: "fixture-network".into(),
    }
}
fn event(at: u64, conversation: &str, input: u64) -> Value {
    json!({"timestamp":at.to_string(),"conversationId":conversation,"tokenUsage":{"inputTokens":input,"outputTokens":3,"cacheReadTokens":100,"cacheWriteTokens":7}})
}
fn abandon(ledger: &Arc<Ledger>, config: &Open, conversation: &str, since: u64) {
    let mut book = ledger.begin(config, conversation, since);
    book.pending();
}

#[tokio::test]
async fn an_abandoned_tool_run_is_reported_separately_from_the_next_runs_own_prompt() {
    let since = auth::now() - 30_000;
    let now = auth::now();
    let client = Client::new(vec![Reply::json(
        json!({"usageEventsDisplay":[event(since+2_000,"conversation",11),event(now+2_000,"conversation",999),event(since+2_000,"another",999)]}),
    )]);
    let config = config(client.clone(), "one");
    let ledger = Arc::new(Ledger::default());
    abandon(&ledger, &config, "conversation", since);
    let mut current = ledger.begin(&config, "conversation", now);
    current.final_seen();
    let recovered = current.collect().await;
    assert_eq!(
        recovered,
        Counts {
            input: 11,
            output: 3,
            cached: 100,
            written: 7
        }
    );
    let mut usage = json!({"prompt_tokens":500,"completion_tokens":10,"total_tokens":510});
    recovered.attach(&mut usage);
    assert_eq!(usage["prompt_tokens"], 500);
    assert_eq!(usage["magpie_deferred"]["prompt_tokens"], 118);
    current.delivered();
    let mut later = ledger.begin(&config, "conversation", now + 1);
    assert_eq!(later.collect().await, Counts::default());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[test]
fn distinct_same_millisecond_events_are_counted_once_even_when_the_dashboard_repeats_them() {
    let since = auth::now() - 30_000;
    let events =
        json!({"usageEventsDisplay":[event(since+2_000,"same",11),event(since+2_000,"same",22)]});
    let mut scope = Scope::default();
    for _ in 0..2 {
        scope.debts.push_back(Debt {
            since,
            before: Some(auth::now()),
        });
    }
    assert_eq!(
        claim(&mut scope, "same", &events, auth::now()),
        Counts {
            input: 33,
            output: 6,
            cached: 200,
            written: 14
        }
    );
    scope.debts.push_back(Debt {
        since,
        before: None,
    });
    assert_eq!(
        claim(&mut scope, "same", &events, auth::now()),
        Counts::default()
    );
    assert_eq!(scope.debts.len(), 1);
}

#[test]
fn a_same_millisecond_native_admission_cannot_count_its_own_usage_as_old_debt() {
    for abandon_after_admission in [false, true] {
        let since = auth::now() - 30_000;
        let config = config(Client::new(vec![]), "one");
        let ledger = Arc::new(Ledger::default());
        let mut older = ledger.begin(&config, "same", since);
        older.pending();
        if !abandon_after_admission {
            older.abandon();
        }
        let _newer = ledger.begin(&config, "same", since);
        older.abandon();
        let mut state = ledger.state.lock().unwrap();
        let scope = state.scopes.get_mut("one\0same").unwrap();
        assert_eq!(scope.debts[0].before, Some(since));
        assert_eq!(
            claim(
                scope,
                "same",
                &json!({"usageEventsDisplay":[event(since-500,"same",999),event(since+2_000,"same",999)]}),
                auth::now()
            ),
            Counts::default()
        );
        assert_eq!(scope.debts.len(), 1);
    }
}

#[test]
fn semantically_equal_dashboard_counts_have_one_identity_across_json_representations() {
    let since = auth::now() - 30_000;
    let at = since + 2_000;
    let mut scope = Scope::default();
    for _ in 0..2 {
        scope.debts.push_back(Debt {
            since,
            before: None,
        });
    }
    let first = json!({"usageEventsDisplay":[{"timestamp":at.to_string(),"conversationId":"same","tokenUsage":{"inputTokens":"11"}}]});
    let repeated = json!({"usageEventsDisplay":[{"timestamp":at,"conversationId":"same","tokenUsage":{"inputTokens":11,"outputTokens":0,"cacheReadTokens":"0","cacheWriteTokens":0,"displayLabel":"updated"}}]});
    assert_eq!(claim(&mut scope, "same", &first, auth::now()).input, 11);
    assert_eq!(
        claim(&mut scope, "same", &repeated, auth::now()),
        Counts::default()
    );
    assert_eq!(scope.debts.len(), 1);
    assert_eq!(
        claim(
            &mut scope,
            "same",
            &json!({"usageEventsDisplay":[event(at,"same",22)]}),
            auth::now()
        )
        .input,
        22
    );
}

#[tokio::test]
async fn carried_usage_expires_independently_of_new_requests_and_repeated_cancellations() {
    let now = auth::now();
    let config = config(Client::new(vec![]), "one");
    let ledger = Arc::new(Ledger::default());
    let mut book = ledger.begin(&config, "same", now);
    {
        let mut state = ledger.state.lock().unwrap();
        let scope = state.scopes.get_mut("one\0same").unwrap();
        scope.carry.add(
            &Counts {
                input: 11,
                ..Counts::default()
            },
            now - RETAIN_MS + 1000,
        );
    }
    assert_eq!(book.collect().await.input, 11);
    book.abandon();
    let _newer = ledger.begin(&config, "same", now + 1);
    let mut state = ledger.state.lock().unwrap();
    let scope = state.scopes.get_mut("one\0same").unwrap();
    assert_eq!(scope.carry.since, Some(now - RETAIN_MS + 1000));
    scope.touched = now + 2000;
    state.prune(now + 2000);
    assert_eq!(state.scopes["one\0same"].carry.counts, Counts::default());
    assert!(state.scopes["one\0same"].carry.since.is_none());
}

#[test]
fn an_evicted_admission_cannot_claim_later_authoritatively_reported_native_usage() {
    let since = auth::now() - 30_000;
    let config = config(Client::new(vec![]), "one");
    let ledger = Arc::new(Ledger::default());
    let mut oldest = ledger.begin(&config, "same", since);
    oldest.pending();
    for index in 1..=65 {
        let mut measured = ledger.begin(&config, "same", since + index * 100);
        measured.final_seen();
        measured.delivered();
    }
    oldest.abandon();
    let mut state = ledger.state.lock().unwrap();
    let scope = state.scopes.get_mut("one\0same").unwrap();
    assert!(scope.debts.is_empty());
    assert_eq!(
        claim(
            scope,
            "same",
            &json!({"usageEventsDisplay":[event(since + 150,"same",999)]}),
            auth::now()
        ),
        Counts::default()
    );
}

#[tokio::test]
async fn repeated_identical_events_keep_their_multiplicity_without_billing_them_twice() {
    let since = auth::now() - 30_000;
    let events =
        json!({"usageEventsDisplay":[event(since+2_000,"same",11),event(since+2_000,"same",11)]});
    let mut scope = Scope::default();
    for _ in 0..2 {
        scope.debts.push_back(Debt {
            since,
            before: None,
        });
    }
    assert_eq!(claim(&mut scope, "same", &events, auth::now()).input, 22);
    scope.debts.push_back(Debt {
        since,
        before: None,
    });
    assert_eq!(
        claim(&mut scope, "same", &events, auth::now()),
        Counts::default()
    );
}

#[tokio::test]
async fn the_same_conversation_and_timestamp_belong_to_each_account_independently() {
    let since = auth::now() - 30_000;
    let ledger = Arc::new(Ledger::default());
    for account in ["one", "two"] {
        let client = Client::new(vec![Reply::json(
            json!({"usageEventsDisplay":[event(since+2_000,"same",11)]}),
        )]);
        let config = config(client.clone(), account);
        abandon(&ledger, &config, "same", since);
        let mut current = ledger.begin(&config, "same", auth::now());
        assert_eq!(current.collect().await.input, 11);
        current.delivered();
        assert_eq!(
            client.requests.lock().unwrap()[0].headers["authorization"],
            format!("Bearer fixture-{account}")
        );
    }
}

#[tokio::test]
async fn final_snapshots_including_zero_and_unstarted_cancellations_never_create_dashboard_debt() {
    let client = Client::new(vec![]);
    let config = config(client.clone(), "one");
    let ledger = Arc::new(Ledger::default());
    {
        let mut complete = ledger.begin(&config, "same", auth::now() - 30_000);
        complete.pending();
        complete.final_seen();
    }
    drop(ledger.begin(&config, "same", auth::now() - 20_000));
    let mut next = ledger.begin(&config, "same", auth::now());
    assert_eq!(next.collect().await, Counts::default());
    assert!(client.requests.lock().unwrap().is_empty());
}

#[tokio::test]
async fn a_collected_result_cancelled_before_delivery_is_carried_to_the_next_reply_once() {
    let since = auth::now() - 30_000;
    let client = Client::new(vec![Reply::json(
        json!({"usageEventsDisplay":[event(since+2_000,"same",11)]}),
    )]);
    let config = config(client.clone(), "one");
    let ledger = Arc::new(Ledger::default());
    abandon(&ledger, &config, "same", since);
    {
        let mut interrupted = ledger.begin(&config, "same", auth::now());
        interrupted.final_seen();
        assert_eq!(interrupted.collect().await.input, 11);
    }
    let mut next = ledger.begin(&config, "same", auth::now() + 1);
    assert_eq!(next.collect().await.input, 11);
    next.delivered();
    let mut last = ledger.begin(&config, "same", auth::now() + 2);
    assert_eq!(last.collect().await, Counts::default());
    assert_eq!(client.requests.lock().unwrap().len(), 1);
}

#[test]
fn newly_admitted_native_runs_bound_old_debts_even_before_the_old_run_is_abandoned() {
    let since = auth::now() - 30_000;
    let config = config(Client::new(vec![]), "one");
    let ledger = Arc::new(Ledger::default());
    let mut older = ledger.begin(&config, "same", since);
    older.pending();
    let _newer = ledger.begin(&config, "same", since + 10_000);
    drop(older);
    let mut state = ledger.state.lock().unwrap();
    let scope = state.scopes.get_mut("one\0same").unwrap();
    assert_eq!(scope.debts[0].before, Some(since + 10_000));
    let events =
        json!({"usageEventsDisplay":[event(since+12_000,"same",999),event(since+2_000,"same",11)]});
    assert_eq!(claim(scope, "same", &events, auth::now()).input, 11);
}

#[tokio::test(start_paused = true)]
async fn dashboard_access_refusals_stop_polling_without_turning_unknown_usage_into_an_estimate() {
    let since = auth::now() - 30_000;
    for status in [400, 401, 403, 404] {
        let client = Client::new(vec![Reply::status(status, json!({"error":"refused"}))]);
        let config = config(client.clone(), "one");
        let ledger = Arc::new(Ledger::default());
        abandon(&ledger, &config, "same", since);
        let mut next = ledger.begin(&config, "same", auth::now());
        assert_eq!(next.collect().await, Counts::default());
        abandon(&ledger, &config, "same", since + 1);
        assert_eq!(next.collect().await, Counts::default());
        assert_eq!(client.requests.lock().unwrap().len(), 1);
    }
}

#[tokio::test(start_paused = true)]
async fn temporary_dashboard_limits_are_retried_without_claiming_a_failed_response() {
    let since = auth::now() - 30_000;
    for status in [429, 500, 503] {
        let client = Client::new(vec![
            Reply::status(status, json!({})),
            Reply::json(json!({"usageEventsDisplay":[event(since+2_000,"same",11)]})),
        ]);
        let config = config(client.clone(), "one");
        let ledger = Arc::new(Ledger::default());
        abandon(&ledger, &config, "same", since);
        let mut next = ledger.begin(&config, "same", auth::now());
        assert_eq!(next.collect().await.input, 11);
        next.delivered();
        assert_eq!(client.requests.lock().unwrap().len(), 2);
    }
}

#[test]
fn malformed_usage_is_not_claimed_and_explicit_dashboard_zero_is_authoritative() {
    let since = auth::now() - 30_000;
    let mut scope = Scope::default();
    scope.debts.push_back(Debt {
        since,
        before: None,
    });
    let unknown = json!({"usageEventsDisplay":[{"timestamp":since+2_000,"conversationId":"same","tokenUsage":{}}]});
    assert_eq!(
        claim(&mut scope, "same", &unknown, auth::now()),
        Counts::default()
    );
    assert_eq!(scope.debts.len(), 1);
    let zero = json!({"usageEventsDisplay":[{"timestamp":(since+2_000).to_string(),"conversationId":"same","tokenUsage":{"inputTokens":"0","outputTokens":0}}]});
    assert_eq!(
        claim(&mut scope, "same", &zero, auth::now()),
        Counts::default()
    );
    assert!(scope.debts.is_empty());
    assert_eq!(scope.claimed.len(), 1);
}

#[tokio::test]
async fn expired_debt_and_capacity_overflow_stay_unknown_and_unretained_runs_create_no_debt() {
    let client = Client::new(vec![]);
    let mut config = config(client.clone(), "one");
    let ledger = Arc::new(Ledger::default());
    for index in 0..70 {
        abandon(&ledger, &config, "same", auth::now() - 10_000 + index);
    }
    assert_eq!(
        ledger.state.lock().unwrap().scopes["one\0same"].debts.len(),
        MAX_DEBTS
    );
    ledger
        .state
        .lock()
        .unwrap()
        .prune(auth::now() + RETAIN_MS + 1);
    config.selector.clear();
    abandon(&ledger, &config, "unretained", auth::now() - 30_000);
    let mut unretained = ledger.begin(&config, "unretained", auth::now());
    assert_eq!(unretained.collect().await, Counts::default());
    assert!(client.requests.lock().unwrap().is_empty());
}
