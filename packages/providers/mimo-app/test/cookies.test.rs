use super::*;
#[test]
fn multiple_cookie_lines_remain_independent_and_never_send_xiaomi_pass_tokens_to_mimo() {
    let mut jar = Jar::default();
    jar.seed("https://account.xiaomi.com", "passToken", "private")
        .unwrap();
    jar.take(
        "https://mimo-server-sgp.xiaomimimo.com/api/sts",
        &[
            "serviceToken=actual; Path=/; Secure; HttpOnly".into(),
            "userId=one; Path=/; Secure".into(),
        ],
    )
    .unwrap();
    let header = jar
        .header("https://mimo-server-sgp.xiaomimimo.com/api/route/chat/completions")
        .unwrap();
    assert!(header.contains("serviceToken=actual"));
    assert!(header.contains("userId=one"));
    assert!(!header.contains("passToken"));
    assert_eq!(
        jar.header("https://account.xiaomi.com/pass/serviceLogin")
            .unwrap(),
        "passToken=private"
    );
}
#[test]
fn secure_domain_path_and_expiry_rules_are_checked_before_cookie_headers_are_built() {
    let mut jar = Jar::default();
    jar.take(
        "https://platform.xiaomimimo.com/api/v1/one",
        &[
            "scoped=value; Domain=.xiaomimimo.com; Path=/api; Secure".into(),
            "evil=bad; Domain=.com; Path=/".into(),
            "deleted=value; Max-Age=0; Path=/".into(),
        ],
    )
    .unwrap();
    assert_eq!(
        jar.header("https://platform.xiaomimimo.com/api/v1/next")
            .unwrap(),
        "scoped=value"
    );
    assert_eq!(
        jar.header("http://platform.xiaomimimo.com/api/v1/next")
            .unwrap(),
        ""
    );
    assert_eq!(
        jar.header("https://platform.xiaomimimo.com/apix").unwrap(),
        ""
    );
    assert_eq!(jar.header("https://evil.example/api").unwrap(), "");
}
#[test]
fn max_age_takes_precedence_and_longer_paths_win_cookie_name_collisions() {
    let mut jar = Jar::default();
    jar.take(
        "https://platform.xiaomimimo.com/api/one",
        &[
            "token=root; Path=/".into(),
            "token=specific; Path=/api; Max-Age=60; Expires=Thu, 01 Jan 1970 00:00:00 GMT".into(),
        ],
    )
    .unwrap();
    assert_eq!(
        jar.map("https://platform.xiaomimimo.com/api/next").unwrap()["token"],
        "specific"
    );
    assert_eq!(
        jar.header("https://platform.xiaomimimo.com/api/next")
            .unwrap(),
        "token=specific; token=root"
    );
}
#[test]
fn serialized_cookie_maps_have_a_stable_order_and_reject_header_injection() {
    let cookies = BTreeMap::from([
        ("other".into(), "last".into()),
        ("userId".into(), "one".into()),
        ("serviceToken".into(), "first".into()),
    ]);
    assert_eq!(
        header(&cookies).unwrap(),
        "serviceToken=first; userId=one; other=last"
    );
    assert!(header(&BTreeMap::from([(
        "token".into(),
        "bad\r\nHeader: value".into()
    )]))
    .is_err());
}
