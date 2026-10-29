use super::*;
use crate::tests::credentials;
#[test]
fn shuffled_wire_codec_round_trips_binary_utf8_and_uses_the_clients_padding() {
    for input in [
        "",
        "a",
        "ab",
        "hello",
        "你好，Qoder 🦜",
        r#"{"messages":[{"content":"a\nb"}]}"#,
    ] {
        let wire = encode(input);
        assert!(wire.is_ascii());
        assert_eq!(wire.len() % 4, 0);
        assert_eq!(decode(&wire).unwrap(), input);
    }
    assert!(decode("invalid?").is_err());
}
#[test]
fn cosy_envelope_binds_the_encoded_body_and_algo_stripped_path() {
    let url = "https://api3.qoder.sh/algo/api/v2/model/list?Encode=1";
    let headers = headers(url, &credentials(), "wire-body").unwrap();
    assert_eq!(headers["cosy-version"], VERSION);
    assert_eq!(headers["cosy-clienttype"], "10");
    assert_eq!(headers["cosy-machinetoken"], credentials()["machineId"]);
    assert!(!headers["authorization"].contains("job-one"));
    let auth = headers["authorization"]
        .strip_prefix("Bearer COSY.")
        .unwrap()
        .split('.')
        .collect::<Vec<_>>();
    assert_eq!(auth.len(), 2);
    let payload: Value = serde_json::from_slice(&STANDARD.decode(auth[0]).unwrap()).unwrap();
    assert_eq!(payload["cosyVersion"], VERSION);
    assert_eq!(STANDARD.decode(&headers["cosy-key"]).unwrap().len(), 128);
    assert_eq!(
        STANDARD.decode(text(&payload, "info")).unwrap().len() % 16,
        0
    );
    let expected = format!(
        "{:x}",
        Md5::digest(
            format!(
                "{}\n{}\n{}\nwire-body\n/api/v2/model/list",
                auth[0], headers["cosy-key"], headers["cosy-date"]
            )
            .as_bytes()
        )
    );
    assert_eq!(auth[1], expected);
}
