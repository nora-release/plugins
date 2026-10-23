use super::*;
use crate::tests::{configured, credentials, Client};
#[test]
fn usage_reads_each_credit_pool_without_treating_quoted_or_negative_numbers_as_allowances() {
    let value=parse(&json!({"displayMode":"qoder","qoderUsage":{"userType":"Pro","expiresAt":"2026-11-01T00:00:00Z","userQuota":{"total":100,"used":40},"addOnQuota":{"cap":20,"remaining":5},"orgResourcePackage":{"total":"10","used":2},"dedicatedResourcePackages":[{"total":10,"used":-1},{"name":"Dedicated","total":10,"remaining":6}]}})).unwrap();
    assert_eq!(value["plan"], "Pro");
    let windows = value["windows"].as_array().unwrap();
    assert_eq!(windows.len(), 3);
    assert_eq!(windows[0]["used"], 40.0);
    assert_eq!(windows[1]["amount"], 15.0);
    assert_eq!(windows[2]["amount"], 4.0);
    assert_eq!(value["until"], "2026-11-01T00:00:00.000Z");
    assert_eq!(
        parse(&json!({"displayMode":"enterprise"})).unwrap()["plan"],
        "Enterprise"
    );
    assert!(parse(&json!({"displayMode":"unknown"})).is_err());
    assert_eq!(date(&json!(1700000000)), date(&json!(1700000000000_u64)));
}
#[tokio::test]
async fn cn_checkin_uses_only_the_accounts_device_token_and_requires_a_real_claim_confirmation() {
    let client = Client::new(vec![
        (
            200,
            json!({"campaigns":[{"campaignId":"campaign/a","actionType":"CLAIM_BENEFIT","claimStatus":"CLAIMABLE","benefit":{"amount":100}}]}),
        ),
        (200, json!({"status":"CLAIMED","benefit":{"amount":101}})),
    ]);
    let provider = configured(Site::China, client.clone());
    let result = checkin(&provider, client.as_ref(), credentials())
        .await
        .unwrap();
    assert_eq!(result["outcome"], "claimed");
    assert_eq!(result["credit"], 101.0);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert_eq!(requests[0].headers["authorization"], "Bearer device-one");
    assert!(!requests[0].headers.contains_key("cosy-machinetoken"));
    assert!(requests[1].url.ends_with("/campaign%2Fa/claim"));
    assert_eq!(requests[1].body.as_deref(), Some(b"{}".as_slice()));
}
#[tokio::test]
async fn unconfirmed_or_unavailable_benefits_never_report_claimed() {
    for (claim_status, expected) in [("INELIGIBLE", false), ("UNKNOWN", false)] {
        let client = Client::new(vec![
            (
                200,
                json!({"campaigns":[{"campaignId":"daily","actionType":"CLAIM_BENEFIT","claimStatus":"CLAIMABLE"}]}),
            ),
            (200, json!({"status":claim_status})),
        ]);
        let provider = configured(Site::China, client.clone());
        assert_eq!(
            checkin(&provider, client.as_ref(), credentials())
                .await
                .is_ok(),
            expected
        );
    }
    let client = Client::new(vec![(200, json!({"campaigns":[]}))]);
    let provider = configured(Site::China, client.clone());
    assert_eq!(
        checkin(&provider, client.as_ref(), credentials())
            .await
            .unwrap()["outcome"],
        "inactive"
    );
}
#[tokio::test]
async fn a_refused_account_page_refresh_does_not_expire_the_independent_chat_token() {
    let client = Client::new(vec![(401, json!({})), (403, json!({}))]);
    let provider = configured(Site::International, client.clone());
    let error = fetch(&provider, client.as_ref(), credentials())
        .await
        .unwrap_err();
    assert_eq!(error.code, "upstream_transport_error");
    assert!(error.message.contains("Chat can continue"));
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 2);
    assert!(requests[1].url.ends_with("/api/v1/deviceToken/refresh"));
}
#[cfg(unix)]
#[tokio::test]
async fn installed_native_identity_receives_uid_via_stdin_and_validates_its_real_output() {
    use std::os::unix::fs::PermissionsExt;
    let directory = std::env::temp_dir().join(format!("qoder-identity-{}", id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("runtime-info");
    let input = directory.join("stdin.json");
    let args = directory.join("args.txt");
    let script=format!("#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\ncat > '{}'\nprintf '%s\\n' '{{\"machineToken\":\"native-token\",\"machineType\":\"native-type\"}}'\n",args.display(),input.display());
    std::fs::write(&path, script).unwrap();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
    let headers = runtime_identity(&path, "private-user-id").await.unwrap();
    assert_eq!(headers["cosy-machinetoken"], "native-token");
    let arguments = std::fs::read_to_string(args).unwrap();
    assert!(!arguments.contains("private-user-id"));
    if std::env::consts::OS != "linux" {
        assert!(arguments.contains("--account-stdin"));
        assert_eq!(
            serde_json::from_slice::<Value>(&std::fs::read(input).unwrap()).unwrap()["account"],
            "private-user-id"
        );
    }
    std::fs::remove_dir_all(directory).unwrap();
}
