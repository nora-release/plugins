use super::*;
use chrono::{DateTime, TimeZone, Utc};

#[test]
fn tc3_signature_matches_tencent_documentation_byte_for_byte() {
    // The Tencent TC3 documentation oracle includes JSON unicode escapes and
    // spaces; changing those bytes changes the documented signature.
    let body=br#"{"Limit": 1, "Filters": [{"Values": ["\u672a\u547d\u540d"], "Name": "instance-name"}]}"#;
    assert_eq!(
        hash(body),
        "35e9c5b0e3ae67532d3c9f17ead6c90222632e5b1ff7f6e89887f1398934f064"
    );
    let id = format!("AKID{}", "*".repeat(32));
    let secret = "*".repeat(32);
    let now = Utc.timestamp_opt(1551113065, 0).unwrap();
    assert_eq!(sign(&id,&secret,"cvm","cvm.tencentcloudapi.com","DescribeInstances",body,now),
        format!("TC3-HMAC-SHA256 Credential={id}/2019-02-25/cvm/tc3_request, SignedHeaders=content-type;host;x-tc-action, Signature=10b1a37a7301a02ca19a647ad722d5e43b4b3cff309d421d85b46093f6ab6c4f"));
}
#[test]
fn timezone_and_header_casing_follow_the_signed_utc_contract() {
    let local = DateTime::parse_from_rfc3339("2019-02-25T23:24:25+08:00")
        .unwrap()
        .with_timezone(&Utc);
    let utc = DateTime::parse_from_rfc3339("2019-02-25T15:24:25Z")
        .unwrap()
        .with_timezone(&Utc);
    assert_eq!(
        sign(
            "id",
            "secret",
            "tokenhub",
            HOST,
            "DescribeTokenPlan",
            b"{}",
            local
        ),
        sign(
            "id",
            "secret",
            "tokenhub",
            HOST,
            "describetokenplan",
            b"{}",
            utc
        )
    );
}
