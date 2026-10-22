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
