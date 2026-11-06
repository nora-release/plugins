use super::*;

#[test]
fn huawei_signature_matches_the_independent_upstream_known_answer() {
    let credentials =
        serde_json::json!({"access_key_id":"AK","secret_access_key":"SK","security_token":"ST"});
    let signed = huawei(
        &credentials,
        "POST",
        "https://example.com/api/v2/chat/completions?b=2&a=1",
        br#"{"m":1}"#,
        BTreeMap::from([("maas_type".into(), "benefit".into())]),
        "20260101T000000Z",
    )
    .unwrap();
    assert_eq!(signed["authorization"],"SDK-HMAC-SHA256 Access=AK,SignedHeaders=content-type;host;maas_type;x-sdk-content-sha256;x-sdk-date;x-security-token,Signature=f33e9f9e8fa23cb4dbac42f858e310657f72cc67179a098e0b3a2fa9a693e161");
    assert!(!signed.contains_key("host"));
    assert_eq!(
        canonical_query(&url::Url::parse("https://h/p?agent_id=a b&x=(1)*&x=!&z=0").unwrap()),
        "agent_id=a%20b&x=%21&x=%281%29%2A&z=0"
    );
}

#[test]
fn dpop_is_a_verifiable_fixed_width_es256_proof_without_the_private_key() {
    use p256::ecdsa::{signature::Verifier, VerifyingKey};
    let private = key();
    let proof = dpop(
        &private,
        "POST",
        "https://sts.example/tokens",
        1_700_000_000_123,
    )
    .unwrap();
    let parts = proof.split('.').collect::<Vec<_>>();
    assert_eq!(parts.len(), 3);
    let header: Value = serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[0]).unwrap()).unwrap();
    let payload: Value =
        serde_json::from_slice(&URL_SAFE_NO_PAD.decode(parts[1]).unwrap()).unwrap();
    assert_eq!(header["typ"], "dpop+jwt");
    assert!(header["jwk"].get("d").is_none());
    assert_eq!(payload["iat"], 1_700_000_000_u64);
    assert_eq!(payload["htm"], "POST");
    assert_eq!(payload["jti"].as_str().unwrap().len(), 64);
    let mut point = vec![4];
    point.extend(URL_SAFE_NO_PAD.decode(text(&private, "x")).unwrap());
    point.extend(URL_SAFE_NO_PAD.decode(text(&private, "y")).unwrap());
    let signature = URL_SAFE_NO_PAD.decode(parts[2]).unwrap();
    assert_eq!(signature.len(), 64);
    VerifyingKey::from_sec1_bytes(&point)
        .unwrap()
        .verify(
            format!("{}.{}", parts[0], parts[1]).as_bytes(),
            &Signature::from_slice(&signature).unwrap(),
        )
        .unwrap();
    let mut wrong = private;
    wrong["x"] = json!("bad");
    assert!(dpop(&wrong, "POST", "https://sts.example/tokens", 0).is_err());
}

#[test]
fn pkce_has_the_original_verifier_and_its_s256_challenge() {
    let (verifier, challenge) = pkce().unwrap();
    assert_eq!(verifier.len(), 64);
    assert_eq!(
        challenge,
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    );
}
