use super::*;
fn incoming(method: &str, path: &str, origin: &str, body: &str) -> Vec<u8> {
    format!("{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:1234\r\nOrigin: {origin}\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).into_bytes()
}
#[test]
fn browser_callback_requires_the_exact_host_allowed_origin_post_path_and_state() {
    let bytes = incoming(
        "POST",
        "/callback",
        ORIGINS[0],
        r#"{"state":"expected","apiKey":"real-key"}"#,
    );
    let request = parse(&bytes, "127.0.0.1:1234").unwrap();
    assert_eq!(approve(&request, "expected").unwrap()["key"], "real-key");
    assert!(approve(&request, "other").is_err());
    assert!(parse(&bytes, "127.0.0.1:9999").is_err());
    assert!(parse(
        &incoming(
            "POST",
            "/callback",
            "https://evil.example",
            r#"{"state":"expected","apiKey":"key"}"#
        ),
        "127.0.0.1:1234"
    )
    .is_err());
    for (method, path) in [("GET", "/callback"), ("POST", "/other")] {
        let request = parse(
            &incoming(
                method,
                path,
                ORIGINS[0],
                r#"{"state":"expected","apiKey":"key"}"#,
            ),
            "127.0.0.1:1234",
        )
        .unwrap();
        assert!(approve(&request, "expected").is_err());
    }
}
#[test]
fn callback_rejects_duplicate_fields_and_reports_a_real_denial() {
    let mut request = parse(
        &incoming(
            "POST",
            "/callback",
            ORIGINS[0],
            r#"{"state":"expected","error":"access_denied"}"#,
        ),
        "127.0.0.1:1234",
    )
    .unwrap();
    assert_eq!(
        approve(&request, "expected").unwrap_err().code,
        "authorization_denied"
    );
    request.headers.insert(
        "content-type".into(),
        "application/x-www-form-urlencoded".into(),
    );
    request.body = b"state=expected&apiKey=one&apiKey=two".to_vec();
    assert!(approve(&request, "expected").is_err());
}
#[test]
fn cli_sources_are_explicit_regular_files_and_are_never_modified() {
    let path = std::env::temp_dir().join(format!("commandcode-source-{}.json", state()));
    let bytes = br#"{"apiKey":"real-key","userId":"uid","refreshToken":"never-consume"}"#;
    std::fs::write(&path, bytes).unwrap();
    let (_, value) = read_source(path.to_str().unwrap()).unwrap();
    assert_eq!(value["key"], "real-key");
    assert!(value.get("refreshToken").is_none());
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(read_source("relative.json").is_err());
    std::fs::remove_file(path).unwrap();
}
#[cfg(unix)]
#[test]
fn cli_sources_reject_symlinks() {
    let path = std::env::temp_dir().join(format!("commandcode-source-{}.json", state()));
    let link = path.with_extension("link");
    std::fs::write(&path, b"{}").unwrap();
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(read_source(link.to_str().unwrap()).is_err());
    std::fs::remove_file(link).unwrap();
    std::fs::remove_file(path).unwrap();
}
#[tokio::test]
async fn real_loopback_posts_accept_studio_approval_without_storing_a_key() {
    let (url, mut browser) = browser().await.unwrap();
    let url = url::Url::parse(&url).unwrap();
    let callback = url::Url::parse(
        &url.query_pairs()
            .find(|(key, _)| key == "callback")
            .unwrap()
            .1,
    )
    .unwrap();
    let state = url
        .query_pairs()
        .find(|(key, _)| key == "state")
        .unwrap()
        .1
        .to_string();
    let host = format!("127.0.0.1:{}", callback.port().unwrap());
    let body = json!({"state":state,"apiKey":"approved-key"}).to_string();
    let mut socket = tokio::net::TcpStream::connect(&host).await.unwrap();
    socket.write_all(format!("POST /callback HTTP/1.1\r\nHost: {host}\r\nOrigin: https://commandcode.ai\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",body.len()).as_bytes()).await.unwrap();
    let mut response = Vec::new();
    socket.read_to_end(&mut response).await.unwrap();
    assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"));
    let result = (&mut browser.receive).await.unwrap().unwrap();
    assert_eq!(result["key"], "approved-key");
}
