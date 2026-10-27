use super::*;
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc,
};
use std::time::Duration;
#[path = "../../runtime/test/system_tls_support.rs"]
mod fixture;
#[path = "ipv4_support.rs"]
mod support;
use support::{Behavior, Endpoint};

struct Site {
    host: &'static str,
    ca: fixture::Authority,
    v4: Endpoint,
    v6: Endpoint,
}
impl Site {
    async fn new(host: &'static str, behavior: Behavior) -> Self {
        let ca = fixture::Authority::new();
        let v4 = Endpoint::v4(Behavior::Tls(ca.acceptor(&[host], false), true)).await;
        let v6 = Endpoint::v6(v4.address.port(), behavior).await;
        Self { host, ca, v4, v6 }
    }
    fn builder(&self) -> reqwest::ClientBuilder {
        self.configure(http_client_builder(Some(&NetworkProxy::Direct)).unwrap())
    }
    fn configure(&self, builder: reqwest::ClientBuilder) -> reqwest::ClientBuilder {
        builder
            .use_preconfigured_tls(tls::http_client_config([self.ca.der()]).unwrap())
            .resolve_to_addrs(self.host, &[self.v6.address, self.v4.address])
            .timeout(Duration::from_secs(3))
    }
    fn url(&self) -> String {
        format!("https://{}:{}/messages", self.host, self.v4.address.port())
    }
}

#[tokio::test]
async fn eof_and_reset_recover_buffered_and_streaming_posts_without_replaying_bodies() {
    for behavior in [Behavior::Eof, Behavior::Reset] {
        let site = Site::new("post.nora.test", behavior).await;
        let client = site.builder().build().unwrap();
        let response = client
            .post(site.url())
            .body("buffered message")
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "reply");
        let stream_site = Site::new("stream.nora.test", Behavior::Eof).await;
        let polled = Arc::new(AtomicUsize::new(0));
        let count = polled.clone();
        let body = reqwest::Body::wrap_stream(futures_util::stream::once(async move {
            count.fetch_add(1, Ordering::SeqCst);
            Ok::<_, std::io::Error>(b"streaming message".as_slice())
        }));
        let response = stream_site
            .builder()
            .build()
            .unwrap()
            .post(stream_site.url())
            .header(reqwest::header::CONTENT_LENGTH, 17)
            .body(body)
            .send()
            .await
            .unwrap();
        assert_eq!(response.text().await.unwrap(), "reply");
        assert_eq!(site.v6.count(), 1);
        assert_eq!(site.v4.count(), 1);
        assert_eq!(site.v4.bodies(), [b"buffered message".to_vec()]);
        assert_eq!(stream_site.v6.count(), 1);
        assert_eq!(stream_site.v4.count(), 1);
        assert_eq!(stream_site.v4.bodies(), [b"streaming message".to_vec()]);
        assert_eq!(polled.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn received_tls_bytes_and_tls_alerts_are_never_retried_or_remembered() {
    for behavior in [Behavior::PartialTls, Behavior::TlsAlert] {
        let site = Site::new("partial.nora.test", behavior).await;
        let client = site.builder().build().unwrap();
        for _ in 0..2 {
            assert!(client
                .post(site.url())
                .body("message")
                .send()
                .await
                .is_err());
        }
        assert_eq!(site.v6.count(), 2);
        assert_eq!(site.v4.count(), 0);
    }
}

#[tokio::test]
async fn certificate_rejection_does_not_trigger_ipv4_or_disable_validation() {
    let other_ca = fixture::Authority::new();
    let site = Site::new(
        "certificate.nora.test",
        Behavior::Tls(other_ca.acceptor(&["certificate.nora.test"], false), true),
    )
    .await;
    let client = site.builder().build().unwrap();
    for _ in 0..2 {
        assert!(client.get(site.url()).send().await.is_err());
    }
    assert_eq!(site.v6.count(), 2);
    assert_eq!(site.v4.count(), 0);
}

#[tokio::test]
async fn a_completed_tls_handshake_and_sent_http_body_cannot_be_replayed() {
    let ca = fixture::Authority::new();
    let v4 = Endpoint::v4(Behavior::Tls(ca.acceptor(&["sent.nora.test"], false), true)).await;
    let v6 = Endpoint::v6(
        v4.address.port(),
        Behavior::Tls(ca.acceptor(&["sent.nora.test"], false), false),
    )
    .await;
    let site = Site {
        host: "sent.nora.test",
        ca,
        v4,
        v6,
    };
    let client = site.builder().build().unwrap();
    assert!(client
        .post(site.url())
        .body("one sent message")
        .send()
        .await
        .is_err());
    assert_eq!(site.v6.bodies(), [b"one sent message".to_vec()]);
    assert_eq!(site.v6.count(), 1);
    assert_eq!(site.v4.count(), 0);
}

#[tokio::test]
async fn handshake_timeout_and_cancellation_do_not_poison_the_route() {
    let site = Site::new("timeout.nora.test", Behavior::Stall).await;
    let client = site
        .builder()
        .connect_timeout(Duration::from_millis(80))
        .build()
        .unwrap();
    assert!(client
        .get(site.url())
        .send()
        .await
        .unwrap_err()
        .is_timeout());
    let client = site.builder().build().unwrap();
    assert!(tokio::time::timeout(
        Duration::from_millis(80),
        client.post(site.url()).body("message").send()
    )
    .await
    .is_err());
    assert_eq!(site.v6.count(), 2);
    assert_eq!(site.v4.count(), 0);
}

#[tokio::test]
async fn fallback_is_opt_in_and_does_not_change_plain_http() {
    let site = Site::new("disabled.nora.test", Behavior::Eof).await;
    let client = site
        .configure(reqwest::Client::builder().no_proxy())
        .build()
        .unwrap();
    assert!(client.get(site.url()).send().await.is_err());
    assert_eq!(site.v6.count(), 1);
    assert_eq!(site.v4.count(), 0);
    let plain = Site::new("http.nora.test", Behavior::PlainHttpEof).await;
    let client = plain.builder().build().unwrap();
    assert!(client
        .post(plain.url().replacen("https:", "http:", 1))
        .body("plain message")
        .send()
        .await
        .is_err());
    assert_eq!(plain.v6.bodies(), [b"plain message".to_vec()]);
    assert_eq!(plain.v6.count(), 1);
    assert_eq!(plain.v4.count(), 0);
}

#[tokio::test]
async fn http_and_https_proxies_never_use_direct_ipv4_fallback() {
    for scheme in ["http", "https"] {
        let behavior = if scheme == "https" {
            Behavior::Eof
        } else {
            Behavior::PlainHttpEof
        };
        let site = Site::new("proxy.nora.test", behavior).await;
        let proxy = format!("{scheme}://{}:{}", site.host, site.v4.address.port());
        let client = site
            .builder()
            .proxy(reqwest::Proxy::all(proxy).unwrap())
            .build()
            .unwrap();
        assert!(client
            .post("https://origin.nora.test/messages")
            .body("secret")
            .send()
            .await
            .is_err());
        assert_eq!(site.v6.count(), 1);
        assert_eq!(site.v4.count(), 0);
    }
}

#[tokio::test]
async fn ipv4_failure_cannot_be_mistaken_for_an_ipv6_failure() {
    let ca = fixture::Authority::new();
    let v4 = Endpoint::v4(Behavior::Eof).await;
    let v6 = Endpoint::v6(
        v4.address.port(),
        Behavior::Tls(ca.acceptor(&["ipv4.nora.test"], false), true),
    )
    .await;
    let site = Site {
        host: "ipv4.nora.test",
        ca,
        v4,
        v6,
    };
    let client = site
        .builder()
        .resolve_to_addrs(site.host, &[site.v4.address, site.v6.address])
        .build()
        .unwrap();
    for _ in 0..2 {
        assert!(client.get(site.url()).send().await.is_err());
    }
    assert_eq!(site.v4.count(), 2);
    assert_eq!(site.v6.count(), 0);
}

#[tokio::test]
async fn an_ipv6_source_binding_and_literal_address_are_not_rewritten() {
    let site = Site::new("bind.nora.test", Behavior::Eof).await;
    let client = site
        .builder()
        .local_address(IpAddr::V6(Ipv6Addr::LOCALHOST))
        .build()
        .unwrap();
    assert!(client.get(site.url()).send().await.is_err());
    assert_eq!(site.v6.count(), 1);
    assert_eq!(site.v4.count(), 0);
    let client = site.builder().build().unwrap();
    assert!(client
        .get(format!("https://[::1]:{}/", site.v6.address.port()))
        .send()
        .await
        .is_err());
    assert_eq!(site.v6.count(), 2);
    assert_eq!(site.v4.count(), 0);
}

#[tokio::test]
async fn a_failed_ipv4_fallback_has_only_one_attempt_and_no_request_body_is_sent() {
    let ca = fixture::Authority::new();
    let v4 = Endpoint::v4(Behavior::Eof).await;
    let v6 = Endpoint::v6(v4.address.port(), Behavior::Eof).await;
    let site = Site {
        host: "both.nora.test",
        ca,
        v4,
        v6,
    };
    let client = site.builder().build().unwrap();
    assert!(client
        .post(site.url())
        .body("message")
        .send()
        .await
        .is_err());
    assert_eq!(site.v6.count(), 1);
    assert_eq!(site.v4.count(), 1);
    assert!(site.v4.bodies().is_empty());
    assert!(site.v6.bodies().is_empty());
}

#[tokio::test]
async fn preference_is_port_specific_and_does_not_affect_concurrent_ipv6_connections() {
    let one = Site::new("ports.nora.test", Behavior::Eof).await;
    let two = Site::new("ports.nora.test", Behavior::Reset).await;
    let ca = fixture::Authority::new();
    let v6 = Endpoint::v6(
        0,
        Behavior::Tls(ca.acceptor(&["v6only.nora.test"], false), true),
    )
    .await;
    let v6_client = http_client_builder(Some(&NetworkProxy::Direct))
        .unwrap()
        .use_preconfigured_tls(tls::http_client_config([ca.der()]).unwrap())
        .resolve_to_addrs("v6only.nora.test", &[v6.address])
        .build()
        .unwrap();
    let one_client = one.builder().build().unwrap();
    let two_client = two.builder().build().unwrap();
    let (a, b, c) = tokio::join!(
        one_client.get(one.url()).send(),
        two_client.get(two.url()).send(),
        v6_client
            .get(format!("https://v6only.nora.test:{}/", v6.address.port()))
            .send()
    );
    assert_eq!(a.unwrap().text().await.unwrap(), "reply");
    assert_eq!(b.unwrap().text().await.unwrap(), "reply");
    assert_eq!(c.unwrap().text().await.unwrap(), "reply");
    assert_eq!(one.v6.count(), 1);
    assert_eq!(two.v6.count(), 1);
    assert_eq!(v6.count(), 1);
}
