use super::*;
#[test]
fn authenticated_uid_stays_stable_across_token_and_email_changes_and_isolates_the_region() {
    let mut credentials =
        json!({"uid":"real-user","email":"display@example.invalid","access":"one"});
    assign(Site::China, &mut credentials).unwrap();
    let identity = credentials["accountIdentity"].clone();
    credentials["email"] = json!("changed@example.invalid");
    credentials["access"] = json!("rotated");
    assign(Site::China, &mut credentials).unwrap();
    assert_eq!(credentials["accountIdentity"], identity);
    assign(Site::Global, &mut credentials).unwrap();
    assert_ne!(credentials["accountIdentity"], identity);
    assert!(assign(Site::China, &mut json!({"email":"display@example.invalid"})).is_err());
}
#[tokio::test]
async fn saved_uid_metadata_is_pure_and_does_not_scan_the_desktop() {
    let client = crate::tests::Client::new(vec![]);
    let provider = crate::tests::configured(Site::China, client.clone());
    let mut context = CallContext::default();
    context.account = "saved".into();
    context.credentials = Some(crate::tests::credentials());
    let result = provider.accounts(context).await.unwrap();
    assert!(!result["credentials"]["accountIdentity"]["subject"]
        .as_str()
        .unwrap()
        .is_empty());
    assert!(client.requests.lock().unwrap().is_empty());
}
