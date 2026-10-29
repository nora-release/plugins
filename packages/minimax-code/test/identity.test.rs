use super::*;
#[test]
fn verified_real_user_id_is_stable_across_rotation_and_separates_regions() {
    let mut credentials =
        json!({"realUserID":"real-user","email":"display@example.invalid","access":"one"});
    let cn = Site::for_id("minimax-code").unwrap();
    assign(cn, &mut credentials).unwrap();
    let identity = credentials["accountIdentity"].clone();
    credentials["email"] = json!("changed@example.invalid");
    credentials["access"] = json!("rotated");
    assign(cn, &mut credentials).unwrap();
    assert_eq!(credentials["accountIdentity"], identity);
    assign(
        Site::for_id("minimax-code-global").unwrap(),
        &mut credentials,
    )
    .unwrap();
    assert_ne!(credentials["accountIdentity"], identity);
    let mut unverified =
        json!({"accountId":"display@example.invalid","email":"display@example.invalid"});
    assert!(!assign(cn, &mut unverified).unwrap());
    assert!(unverified.get("accountIdentity").is_none());
}
