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
