use super::*;
use base64::Engine;
fn jwt(value: Value) -> String {
    format!(
        "e30.{}.signature",
        base64::engine::general_purpose::URL_SAFE_NO_PAD
            .encode(serde_json::to_vec(&value).unwrap())
    )
}
#[test]
fn uid_preserves_large_hexadecimal_account_identifiers_without_rounding() {
    assert_eq!(
        uid(&json!({"sid":"ffffffffffffffff"})),
        "18446744073709551615"
    );
    assert_eq!(
        uid(&json!({"iss":"https://issuer.example","sub":"real-user"})),
        "real-user"
    );
}
#[test]
fn expired_tokens_and_header_injection_are_rejected() {
    assert!(credential(&jwt(json!({"sid":"ab","exp":1})), "", "manual").is_err());
    assert!(credential("token\r\nX: evil", "", "manual").is_err());
}
#[test]
fn callbacks_require_the_exact_loopback_host_path_method_and_state() {
    let origin = "127.0.0.1:12345";
    let good =
        format!("GET /auth/callback?code=real&state=expected HTTP/1.1\r\nHost: {origin}\r\n\r\n");
    assert_eq!(callback_code(&good, origin, "expected").unwrap(), "real");
    for bad in [
        good.replace("Host: 127.0.0.1:12345", "Host: attacker.example"),
        good.replace("GET ", "POST "),
        good.replace("state=expected", "state=wrong"),
        good.replace("/auth/callback", "/wrong"),
        good.replace("code=real", "code=one&code=two"),
        good.replace("state=expected", "state=expected&state=expected"),
    ] {
        assert!(callback_code(&bad, origin, "expected").is_err());
    }
}
#[test]
fn manual_callbacks_reject_a_conflicting_state_and_keep_token_paste_explicit() {
    assert!(manual(
        "office-raccoon://auth/callback?code=one&state=wrong",
        "expected"
    )
    .is_err());
    assert_eq!(
        manual(
            "office-raccoon://auth/callback?code=one&state=expected",
            "expected"
        )
        .unwrap()["code"],
        "one"
    );
    let token = jwt(json!({"sid":"01","exp":core::now()/1000+3600}));
    assert_eq!(
        manual(&token, "expected").unwrap()["direct"]["access"],
        token
    );
}
#[test]
fn selected_desktop_files_are_read_without_being_modified() {
    let path = std::env::temp_dir().join(format!("raccoon-source-{}.json", state()));
    let bytes=serde_json::to_vec(&json!({"access_token":jwt(json!({"sid":"ff","name":"Work","exp":core::now()/1000+3600})),"refresh_token":"never-spend-desktop-refresh"})).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let value = read_source(path.to_str().unwrap()).unwrap();
    assert_eq!(value["uid"], "255");
    assert_eq!(value["refresh"], "");
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    std::fs::remove_file(path).unwrap();
}
#[cfg(unix)]
#[test]
fn desktop_source_rejects_symlinks_and_oversized_files() {
    let path = std::env::temp_dir().join(format!("raccoon-source-{}.json", state()));
    let link = path.with_extension("link");
    std::fs::write(&path, b"{}").unwrap();
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(read_source(link.to_str().unwrap()).is_err());
    std::fs::remove_file(link).unwrap();
    std::fs::write(&path, vec![b' '; 256 * 1024 + 1]).unwrap();
    assert!(read_source(path.to_str().unwrap()).is_err());
    std::fs::remove_file(path).unwrap();
}
#[tokio::test]
async fn browser_listener_only_completes_for_its_own_real_callback() {
    let (url, mut pending) = browser("a".repeat(64)).await.unwrap();
    let parsed = url::Url::parse(&url).unwrap();
    let redirect = parsed
        .query_pairs()
        .find(|(key, _)| key == "redirect")
        .unwrap()
        .1
        .to_string();
    let redirect = url::Url::parse(&redirect).unwrap();
    let host = format!("127.0.0.1:{}", redirect.port().unwrap());
    let mut socket = tokio::net::TcpStream::connect(&host).await.unwrap();
    socket
        .write_all(
            format!(
                "GET /auth/callback?code=approved&state={} HTTP/1.1\r\nHost: {host}\r\n\r\n",
                "a".repeat(64)
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    let mut response = Vec::new();
    socket.read_to_end(&mut response).await.unwrap();
    assert!(String::from_utf8_lossy(&response).starts_with("HTTP/1.1 200"));
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(2), &mut pending.receive)
            .await
            .unwrap()
            .unwrap()
            .unwrap(),
        "approved"
    );
}
