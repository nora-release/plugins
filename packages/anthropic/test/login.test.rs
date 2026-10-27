use super::*;
#[test]
fn browser_opener_only_accepts_official_https_oauth_pages() {
    for value in [
        "http://claude.ai/oauth/authorize",
        "https://attacker.invalid/oauth/authorize",
        "https://claude.ai@attacker.invalid/oauth/authorize",
        "https://user:password@claude.ai/oauth/authorize",
    ] {
        assert!(!cli::login_url(&url::Url::parse(value).unwrap()));
    }
    assert!(cli::login_url(
        &url::Url::parse("https://claude.ai/oauth/authorize?state=secret").unwrap()
    ));
}

#[test]
fn pasted_callback_keeps_the_original_listener_address_even_for_ipv6() {
    for endpoint in [
        "http://[::1]:53191/callback",
        "http://localhost:53191/callback",
    ] {
        let mut url = url::Url::parse("https://claude.ai/oauth/authorize").unwrap();
        url.query_pairs_mut()
            .append_pair("state", "matching-state")
            .append_pair("redirect_uri", endpoint);
        let page = Page::new(url.into(), true);
        let target = page
            .callback_target("https://127.0.0.1:53191/callback?code=code&state=matching-state")
            .unwrap();
        assert_eq!(
            target.host_str(),
            url::Url::parse(endpoint).unwrap().host_str()
        );
        assert_eq!(target.port(), Some(53191));
        assert_eq!(target.scheme(), "http");
    }
}

#[test]
fn pasted_callback_rejects_ambiguous_fields_and_external_redirects() {
    let mut url = url::Url::parse("https://claude.ai/oauth/authorize").unwrap();
    url.query_pairs_mut()
        .append_pair("state", "matching-state")
        .append_pair("redirect_uri", "http://localhost:53191/callback");
    let page = Page::new(url.into(), true);
    for query in [
        "code=code&state=matching-state&state=matching-state",
        "code=code&code=other&state=matching-state",
        "code=&state=matching-state",
        "code=code&state=other",
        "code=code",
        "code=code%0Aother&state=matching-state",
    ] {
        assert!(page
            .callback_target(&format!("http://localhost:53191/callback?{query}"))
            .is_err());
    }
    for target in [
        "http://127.0.0.2:53191/callback",
        "http://user@localhost:53191/callback",
        "https://example.invalid:53191/callback",
        "http://localhost:53192/callback",
        "http://localhost:53191/wrong",
    ] {
        assert!(page
            .callback_target(&format!("{target}?code=code&state=matching-state"))
            .is_err());
    }
    assert!(page
        .callback_target(&format!(
            "http://localhost:53191/callback?code={}&state=matching-state",
            "x".repeat(8193)
        ))
        .is_err());
}
