use super::*;
use crate::fixture::Client;
use rsa::pkcs1::DecodeRsaPublicKey;
#[test]
fn zed_rsa_oaep_sha256_and_legacy_pkcs1_ciphertexts_decrypt_without_exposing_invalid_data() {
    let key = RsaPrivateKey::new(&mut rand::rngs::OsRng, 1024).unwrap();
    let public = RsaPublicKey::from(&key);
    for ciphertext in [
        public
            .encrypt(
                &mut rand::rngs::OsRng,
                Oaep::new::<sha2::Sha256>(),
                b"private-token",
            )
            .unwrap(),
        public
            .encrypt(&mut rand::rngs::OsRng, Pkcs1v15Encrypt, b"private-token")
            .unwrap(),
    ] {
        assert_eq!(
            decrypt(
                &key,
                &base64::engine::general_purpose::URL_SAFE.encode(ciphertext)
            )
            .unwrap(),
            "private-token"
        );
    }
    let error = decrypt(&key, "not-valid-private-token").unwrap_err();
    assert!(!error.message.contains("private-token"));
}
#[tokio::test]
async fn native_callback_uses_only_loopback_and_the_advertised_rsa_public_key() {
    let key = RsaPrivateKey::new(&mut rand::rngs::OsRng, 1024).unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let (pending, url) = start_listener(Client::new(vec![]), key, listener).unwrap();
    let url = url::Url::parse(&url).unwrap();
    assert_eq!(url.origin().ascii_serialization(), "https://zed.dev");
    let query: std::collections::BTreeMap<_, _> = url.query_pairs().into_owned().collect();
    let public = RsaPublicKey::from_pkcs1_der(
        &base64::engine::general_purpose::URL_SAFE
            .decode(&query["native_app_public_key"])
            .unwrap(),
    )
    .unwrap();
    let encrypted = public
        .encrypt(
            &mut rand::rngs::OsRng,
            Oaep::new::<sha2::Sha256>(),
            b"browser-private-access",
        )
        .unwrap();
    let encrypted = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(encrypted);
    let mut socket = tokio::net::TcpStream::connect((
        std::net::Ipv4Addr::LOCALHOST,
        query["native_app_port"].parse::<u16>().unwrap(),
    ))
    .await
    .unwrap();
    socket.write_all(format!("GET /callback?user_id=77&access_token={encrypted} HTTP/1.1\r\nHost: localhost\r\n\r\n").as_bytes()).await.unwrap();
    let mut reply = Vec::new();
    socket.read_to_end(&mut reply).await.unwrap();
    assert!(String::from_utf8(reply).unwrap().contains("302 Found"));
    let account = pending.complete().await.unwrap();
    assert_eq!(account.access, "browser-private-access");
    assert_eq!(account.user, "77");
}
#[tokio::test]
async fn cancelling_sign_in_closes_its_callback_listener() {
    let key = RsaPrivateKey::new(&mut rand::rngs::OsRng, 1024).unwrap();
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let port = listener.local_addr().unwrap().port();
    let (pending, _) = start_listener(Client::new(vec![]), key, listener).unwrap();
    drop(pending);
    let mut closed = false;
    for _ in 0..10 {
        tokio::task::yield_now().await;
        if tokio::net::TcpStream::connect((std::net::Ipv4Addr::LOCALHOST, port))
            .await
            .is_err()
        {
            closed = true;
            break;
        }
    }
    assert!(closed);
}
