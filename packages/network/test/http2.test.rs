use super::*;
use std::sync::Arc;
#[path = "../../runtime/test/system_tls_support.rs"]
mod fixture;

#[tokio::test]
async fn cursor_http2_negotiates_alpn_and_sends_requests_with_tls12_and_tls13() {
    for version in [&rustls::version::TLS12, &rustls::version::TLS13] {
        let ca = fixture::Authority::new();
        let acceptor = ca.acceptor_version(&["127.0.0.1"], false, &[version]);
        let mut config = acceptor.config().as_ref().clone();
        config.alpn_protocols = vec![b"h2".to_vec()];
        let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let socket = acceptor.accept(socket).await.unwrap();
            assert_eq!(socket.get_ref().1.alpn_protocol(), Some(b"h2".as_slice()));
            assert_eq!(socket.get_ref().1.protocol_version(), Some(version.version));
            let mut session = h2::server::handshake(socket).await.unwrap();
            let (request, mut reply) = session.accept().await.unwrap().unwrap();
            assert_eq!(request.uri().path(), "/http2");
            let mut body = reply.send_response(http::Response::new(()), false).unwrap();
            body.send_data(bytes::Bytes::from_static(b"HTTP/2 secured"), true)
                .unwrap();
            // Drive the connection until the client has consumed the response.
            while session.accept().await.is_some() {}
        });
        let client = http_client_builder(Some(&NetworkProxy::Direct))
            .unwrap()
            .use_preconfigured_tls(tls::http_client_config([ca.der()]).unwrap())
            .http2_prior_knowledge()
            .timeout(std::time::Duration::from_secs(5))
            .build()
            .unwrap();
        let response = client
            .get(format!("https://{address}/http2"))
            .send()
            .await
            .unwrap();
        assert_eq!(response.version(), reqwest::Version::HTTP_2);
        assert_eq!(response.text().await.unwrap(), "HTTP/2 secured");
        drop(client);
        tokio::time::timeout(std::time::Duration::from_secs(5), server)
            .await
            .unwrap()
            .unwrap();
    }
}

#[cfg(feature = "ipv4-fallback")]
#[tokio::test]
async fn automatic_http2_negotiation_survives_a_pre_tls_ipv6_disconnect() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::io::AsyncReadExt;
    let ca = fixture::Authority::new();
    let acceptor = ca.acceptor(&["h2-fallback.nora.test"], false);
    let mut config = acceptor.config().as_ref().clone();
    config.alpn_protocols = vec![b"h2".to_vec()];
    let acceptor = tokio_rustls::TlsAcceptor::from(Arc::new(config));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let v4 = listener.local_addr().unwrap();
    let v6_listener = tokio::net::TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, v4.port()))
        .await
        .unwrap();
    let v6 = v6_listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let count = connections.clone();
    let broken = tokio::spawn(async move {
        while let Ok((mut socket, _)) = v6_listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            let _ = socket.read(&mut [0; 8192]).await;
        }
    });
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let socket = acceptor.accept(socket).await.unwrap();
        assert_eq!(socket.get_ref().1.alpn_protocol(), Some(b"h2".as_slice()));
        let mut connection = h2::server::handshake(socket).await.unwrap();
        let (request, mut reply) = connection.accept().await.unwrap().unwrap();
        assert_eq!(request.method(), http::Method::POST);
        let mut body = request.into_body();
        let handler = tokio::spawn(async move {
            let mut received = Vec::new();
            while let Some(bytes) = body.data().await {
                let bytes = bytes.unwrap();
                received.extend_from_slice(&bytes);
                body.flow_control().release_capacity(bytes.len()).unwrap();
            }
            assert_eq!(received, b"one h2 message");
            let mut response = reply.send_response(http::Response::new(()), false).unwrap();
            response
                .send_data(bytes::Bytes::from_static(b"reply"), true)
                .unwrap();
        });
        while connection.accept().await.is_some() {}
        handler.await.unwrap();
    });
    let client = http_client_builder(Some(&NetworkProxy::Direct))
        .unwrap()
        .use_preconfigured_tls(tls::http_client_config([ca.der()]).unwrap())
        .resolve_to_addrs("h2-fallback.nora.test", &[v6, v4])
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .unwrap();
    let response = client
        .post(format!(
            "https://h2-fallback.nora.test:{}/messages",
            v4.port()
        ))
        .body("one h2 message")
        .send()
        .await
        .unwrap();
    assert_eq!(response.version(), reqwest::Version::HTTP_2);
    assert_eq!(response.text().await.unwrap(), "reply");
    assert_eq!(connections.load(Ordering::SeqCst), 1);
    drop(client);
    tokio::time::timeout(std::time::Duration::from_secs(5), server)
        .await
        .unwrap()
        .unwrap();
    broken.abort();
}
