use super::*;
use crate::tests::Client;

#[test]
fn the_device_flow_is_the_editors_oauth_app() {
    let flow = device_flow();
    assert_eq!(flow.client_id, "Iv1.b507a08c87ecfe98");
    assert_eq!(flow.scope, "read:user");
    assert!(flow
        .headers
        .iter()
        .any(|(name, value)| *name == "editor-version" && value == "vscode/1.140.0"));
}

#[tokio::test]
async fn the_account_is_named_by_its_login() {
    let client = Client::new(vec![(200, json!({"login": "octocat", "id": 1}))]);
    let account = account(client.as_ref(), "gho_1").await.unwrap();
    assert_eq!(account.name, "octocat");
    assert_eq!(
        account.credentials,
        json!({"github": "gho_1", "login": "octocat"})
    );
    let requests = client.requests.lock().unwrap();
    assert_eq!(
        requests[0].headers.get("authorization").map(String::as_str),
        Some("token gho_1")
    );
}
