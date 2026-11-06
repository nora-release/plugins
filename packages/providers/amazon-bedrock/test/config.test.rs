use super::*;
#[test]
fn bearer_keys_and_explicit_aws_credentials_are_distinct_and_region_bound() {
    let manifest = json!({});
    let saved = Account::from_inputs(
        &json!({"method":"apiKey","inputs":{"apiKey":"ABSK-own-key","region":"eu-west-1"}}),
        &manifest,
        false,
    )
    .unwrap();
    let account = Account::saved(&saved, &manifest, false).unwrap();
    assert_eq!(
        account.root(false),
        "https://bedrock-runtime.eu-west-1.amazonaws.com"
    );
    assert!(account.aws.is_none());
    let saved=Account::from_inputs(&json!({"method":"aws","inputs":{"accessKeyId":"AKIA-test","secretAccessKey":"secret","sessionToken":"session","region":"ap-southeast-1"}}),&manifest,false).unwrap();
    let account = Account::saved(&saved, &manifest, false).unwrap();
    assert_eq!(
        account.aws.unwrap().session_token.as_deref(),
        Some("session")
    );
    assert!(Account::saved(&saved, &manifest, true).is_err());
}
#[test]
fn corrupt_regions_and_embedded_aws_header_injection_are_rejected() {
    for region in [
        "evil.example.com",
        "us-east-1/other",
        "us-east-1@evil",
        "us--east-1",
        "US-east-1",
        "",
    ] {
        assert!(validate_region(region).is_err());
    }
    let value = json!({"access":json!({"accessKeyId":"a","secretAccessKey":"s","sessionToken":"token\r\nx: bad"}).to_string(),"region":"us-east-1"});
    assert!(Account::saved(&value, &json!({}), false).is_err());
    let legacy = Account::saved(
        &json!({"access":"AKIA-id:secret:session"}),
        &json!({}),
        false,
    )
    .unwrap();
    assert_eq!(legacy.aws.unwrap().access_key_id, "AKIA-id");
}
