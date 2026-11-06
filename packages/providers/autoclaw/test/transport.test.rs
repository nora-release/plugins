use super::*;

#[test]
fn signed_headers_match_the_reference_clock_vector_and_official_platform_names() {
    let headers = headers("Bearer access-one", 1_700_000_000_000).unwrap();
    assert_eq!(headers["x-auth-timestamp"], "1700000000");
    assert_eq!(headers["x-auth-sign"], "2586ea4cc4191242c9acdc2ee7e96ac8");
    assert_eq!(headers["x-auth-appid"], "100003");
    assert_eq!(headers["authorization"], "Bearer access-one");
    assert_eq!(headers["x-product"], "autoclaw");
    assert_eq!(headers["x-trace-id"].len(), 32);
    assert!(!headers["user-agent"].contains("aarch64"));
    assert!(!headers["user-agent"].contains("x86_64"));
}
