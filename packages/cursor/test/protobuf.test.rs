use super::*;
use serde_json::json;
#[test]
fn typed_values_roundtrip_and_unknown_complete_fields_are_safe() {
    let v = json!({"text":"hello","num":-1.5,"yes":true,"nil":null,"list":[1,"x",{}]});
    let mut message = Pb::new()
        .str(1, "magpie-echo")
        .str(4, "magpie")
        .str(5, "echo");
    for (k, v) in v.as_object().unwrap() {
        message = message.bytes(2, Pb::new().str(1, k).bytes(2, value(v)));
    }
    let (_, got) = arguments(&message.0).unwrap();
    assert_eq!(got, v);
}
#[test]
fn malformed_tails_overflow_invalid_utf8_and_nonfinite_numbers_fail() {
    for bytes in [
        vec![0],
        vec![0x80],
        vec![10, 9, 1],
        vec![15],
        vec![8, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255],
        Pb::new()
            .bytes(
                2,
                Pb::new()
                    .str(1, "bad")
                    .bytes(2, Pb::new().double(2, f64::NAN)),
            )
            .0,
        Pb::new()
            .bytes(2, Pb::new().bytes(1, [255]).bytes(2, value(&json!(1))))
            .0,
    ] {
        assert!(
            arguments(&bytes).is_err() || fields(&bytes).is_err(),
            "{bytes:?}"
        );
    }
}
#[test]
fn compressed_and_fragmented_frames_enforce_boundaries() {
    use std::io::Write;
    let mut gzip = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    gzip.write_all(b"hello").unwrap();
    let mut bytes = frame(&gzip.finish().unwrap());
    bytes[0] = 1;
    let mut frames = Frames::default();
    let mut out = Vec::new();
    for byte in bytes {
        out.extend(frames.push(&[byte]).unwrap());
    }
    assert_eq!(out, vec![(false, b"hello".to_vec())]);
    assert!(frames.finish().is_ok());
    let mut frames = Frames::default();
    frames.push(&[0, 0, 0, 0, 2, 1]).unwrap();
    assert!(frames.finish().is_err());
    assert!(Frames::default().push(&[0, 255, 255, 255, 255]).is_err());
    assert!(Frames::default().push(&[4, 0, 0, 0, 0]).is_err());
}
#[test]
fn overwritten_invalid_oneof_cannot_be_hidden_by_a_later_valid_value() {
    let bad = Pb::new().double(2, f64::NAN).str(3, "valid");
    let envelope = Pb::new().bytes(2, Pb::new().str(1, "x").bytes(2, bad));
    assert!(arguments(&envelope.0).is_err());
    let mut v = json!(null);
    for _ in 0..66 {
        v = json!([v]);
    }
    let deep = Pb::new().bytes(2, Pb::new().str(1, "x").bytes(2, value(&v)));
    assert!(arguments(&deep.0).is_err());
}
