use super::*;
#[test]
fn actual_uid_identity_ignores_email_and_rotation_but_isolates_region_and_enterprise() {
    let mut credentials = json!({"uid":"real-user","email":"same@example.invalid","access":"one"});
    let cn = Site::China.deployment(None).unwrap();
    assign(&mut credentials, &cn).unwrap();
    let original = credentials["accountIdentity"].clone();
    credentials["email"] = json!("changed@example.invalid");
    credentials["access"] = json!("rotated");
    assign(&mut credentials, &cn).unwrap();
    assert_eq!(credentials["accountIdentity"], original);
    assign(
        &mut credentials,
        &Site::International.deployment(None).unwrap(),
    )
    .unwrap();
    assert_ne!(credentials["accountIdentity"], original);
    assign(
        &mut credentials,
        &Site::China.deployment(Some("one")).unwrap(),
    )
    .unwrap();
    let enterprise = credentials["accountIdentity"].clone();
    assign(
        &mut credentials,
        &Site::China.deployment(Some("two")).unwrap(),
    )
    .unwrap();
    assert_ne!(credentials["accountIdentity"], enterprise);
    for value in [
        json!({"email":"same@example.invalid"}),
        json!({"uid":" ","email":"same@example.invalid"}),
    ] {
        assert!(assign(&mut value.clone(), &cn).is_err());
    }
}
#[tokio::test]
async fn saved_uid_metadata_upgrades_without_reading_sources_or_contacting_the_vendor() {
    let client = crate::tests::Client::new(vec![]);
    let provider = crate::tests::configured(Site::China, client.clone());
    let mut context = CallContext::default();
    context.account = "saved".into();
    context.credentials = Some(crate::tests::credentials());
    let result = provider.accounts(context).await.unwrap();
    assert_eq!(result["credentials"]["accountIdentity"]["subject"], "u1");
    assert!(client.requests.lock().unwrap().is_empty());
}
