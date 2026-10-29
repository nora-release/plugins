use super::*;
#[tokio::test]
async fn compiled_plugin_warmup_dispatches_once_to_selected_account_and_retains_credentials() {
    let directory = fixture::Directory::new();
    let selected = directory.0.join("claude-accounts/a");
    let other = directory.0.join("claude-accounts/b");
    let credentials = fixture::profile(&selected);
    fixture::profile(&other);
    let before = std::fs::read(selected.join(".credentials.json")).unwrap();
    let mut host = Host::with_environment(
        &directory.0,
        &[
            ("HOME", directory.0.clone().into_os_string()),
            (
                "CLAUDE_CONFIG_DIR",
                directory.0.join("unrelated").into_os_string(),
            ),
        ],
    )
    .await;
    let id = host
        .send(
            "warmup",
            json!({"account":"a","credentials":credentials,"confirmed":true}),
        )
        .await;
    let result = host.frame(id).await;
    assert_eq!(result["result"]["outcome"], "warmed", "{result}");
    let record: Value =
        serde_json::from_slice(&std::fs::read(selected.join("fixture-warmup.json")).unwrap())
            .unwrap();
    assert_eq!(record["input"], "hi");
    assert!(record["args"].as_array().unwrap().contains(&json!("haiku")));
    assert!(!other.join("fixture-warmup.json").exists());
    assert_eq!(
        std::fs::read(selected.join(".credentials.json")).unwrap(),
        before
    );
    let denied = host
        .send(
            "warmup",
            json!({"account":"a","credentials":credentials,"confirmed":false}),
        )
        .await;
    assert_eq!(host.frame(denied).await["error"]["code"], "invalid_request");
}
