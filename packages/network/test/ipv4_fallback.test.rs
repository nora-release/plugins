use super::*;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
use tokio::io::AsyncReadExt;
use tokio::net::TcpListener;
#[path = "../../runtime/test/system_tls_support.rs"]
mod fixture;

#[tokio::test]
async fn ipv6_disconnect_before_tls_reconnects_once_over_ipv4_and_remembers_the_route() {
    let ca = fixture::Authority::new();
    let site = fixture::Server::start(&ca, &["dual.nora.test"], false, b"reply").await;
    let listener = TcpListener::bind((std::net::Ipv6Addr::LOCALHOST, site.address.port()))
        .await
        .unwrap();
    let v6 = listener.local_addr().unwrap();
    let connections = Arc::new(AtomicUsize::new(0));
    let count = connections.clone();
    let broken = tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            let mut hello = [0; 8192];
            let _ = socket.read(&mut hello).await;
        }
    });
    for method in [reqwest::Method::POST, reqwest::Method::GET] {
        let client = http_client_builder(Some(&NetworkProxy::Direct))
            .unwrap()
            .use_preconfigured_tls(tls::http_client_config([ca.der()]).unwrap())
            .resolve_to_addrs("dual.nora.test", &[v6, site.address])
            .timeout(Duration::from_secs(5))
            .build()
            .unwrap();
        let response = client
            .request(method, site.url("dual.nora.test"))
            .body("one message")
            .send()
            .await;
        assert!(
            response.is_ok(),
            "a direct IPv6 pre-TLS disconnect must recover over IPv4: {response:?}"
        );
        assert_eq!(response.unwrap().text().await.unwrap(), "reply");
    }
    assert_eq!(connections.load(Ordering::SeqCst), 1);
    assert_eq!(site.requests.load(Ordering::SeqCst), 2);
    broken.abort();
}
