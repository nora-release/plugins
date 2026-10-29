use super::*;

async fn activity(host: &mut Host, credentials: Value) -> u64 {
    let id = host
        .send(
            "sessionActivity",
            json!({"account":"work","credentials":credentials}),
        )
        .await;
    let frame = host.frame(id).await;
    assert!(frame.get("error").is_none(), "{frame}");
    frame["result"]["tools"].as_u64().unwrap()
}

#[tokio::test]
async fn session_activity_counts_only_real_parked_runs_in_the_owner_fetch_process() {
    let directory = fixture::Directory::new();
    let credentials = fixture::profile(&directory.0.join("claude-accounts/account-a"));
    let foreign =
        fixture::profile_with_identity(&directory.0.join("claude-accounts/account-b"), "account-b");
    let mut fetch = Host::new(&directory.0).await;
    let mut metadata = Host::new(&directory.0).await;
    assert_eq!(activity(&mut fetch, credentials.clone()).await, 0);
    let mut first = None;
    for session in ["parked-a", "parked-b"] {
        let id = fetch
            .fetch(
                credentials.clone(),
                session,
                "turn",
                vec![user("USE_TOOL")],
                tools(),
                false,
            )
            .await;
        let (status, body) = fetch.response(id).await;
        assert_eq!(status, 200, "{body}");
        let response: Value = serde_json::from_str(&body).unwrap();
        assert_eq!(response["stop_reason"], "tool_use");
        first.get_or_insert(response);
    }
    assert_eq!(activity(&mut fetch, credentials.clone()).await, 2);
    assert_eq!(
        activity(&mut metadata, credentials.clone()).await,
        0,
        "a metadata process must not invent another process's runs"
    );
    assert_eq!(
        activity(&mut fetch, foreign).await,
        0,
        "another account must not see parked runs"
    );
    let response = first.unwrap();
    let tool_id = response["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|part| part["type"] == "tool_use")
        .unwrap()["id"]
        .clone();
    let id=fetch.fetch(credentials.clone(),"parked-a","turn",vec![user("USE_TOOL"),assistant(&response),json!({"role":"user","content":[{"type":"tool_result","tool_use_id":tool_id,"content":"resolved"}]})],tools(),false).await;
    let (status, body) = fetch.response(id).await;
    assert_eq!(status, 200, "{body}");
    assert_eq!(activity(&mut fetch, credentials.clone()).await, 1);
    let disabled = fetch
        .send(
            "sessionActivity",
            json!({"account":"work","credentials":credentials,"subscriptionAccountsEnabled":false}),
        )
        .await;
    let disabled = fetch.frame(disabled).await;
    assert!(disabled.get("error").is_some() || disabled["result"]["tools"] == 0);
}
