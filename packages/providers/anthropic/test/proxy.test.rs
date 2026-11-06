use super::*;
#[tokio::test]
async fn proxy_choices_replace_all_inherited_environment_variants_and_direct_is_explicit() {
    let proxy = Proxy::new(None).await.unwrap();
    for key in KEYS {
        assert_eq!(
            proxy.env[key],
            if key.eq_ignore_ascii_case("no_proxy") {
                "*"
            } else {
                ""
            }
        );
    }
    let proxy = Proxy::new(Some(&NetworkProxy::Proxy {
        url: "https://user:private-password@proxy.example:8443".into(),
    }))
    .await
    .unwrap();
    assert_eq!(
        proxy.env["HTTP_PROXY"],
        "https://user:private-password@proxy.example:8443"
    );
    assert_eq!(proxy.env["https_proxy"], proxy.env["HTTP_PROXY"]);
    assert_eq!(proxy.env["NO_PROXY"], "");
}
#[tokio::test]
async fn socks5h_bridge_performs_real_authentication_and_connect_without_dns_leaks() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut greeting = [0; 3];
        socket.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 1, 2]);
        socket.write_all(&[5, 2]).await.unwrap();
        assert_eq!(socket.read_u8().await.unwrap(), 1);
        let length = socket.read_u8().await.unwrap();
        let mut username = vec![0; length as usize];
        socket.read_exact(&mut username).await.unwrap();
        assert_eq!(username, b"user+");
        let length = socket.read_u8().await.unwrap();
        let mut password = vec![0; length as usize];
        socket.read_exact(&mut password).await.unwrap();
        assert_eq!(password, b"private password");
        socket.write_all(&[1, 0]).await.unwrap();
        let mut header = [0; 4];
        socket.read_exact(&mut header).await.unwrap();
        assert_eq!(header, [5, 1, 0, 3]);
        let size = socket.read_u8().await.unwrap();
        let mut domain = vec![0; size as usize];
        socket.read_exact(&mut domain).await.unwrap();
        assert_eq!(domain, b"unresolvable.private.invalid");
        assert_eq!(socket.read_u16().await.unwrap(), 443);
        socket
            .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 1])
            .await
            .unwrap();
        let mut ping = [0; 4];
        socket.read_exact(&mut ping).await.unwrap();
        assert_eq!(&ping, b"ping");
        socket.write_all(b"pong").await.unwrap();
    });
    let proxy = Proxy::new(Some(&NetworkProxy::Proxy {
        url: format!("socks5h://user%2B:private%20password@{address}"),
    }))
    .await
    .unwrap();
    let url = url::Url::parse(&proxy.env["HTTPS_PROXY"]).unwrap();
    assert_eq!(url.scheme(), "http");
    assert!(!proxy.env.values().any(|value| value.contains("private")));
    let mut client = TcpStream::connect((url.host_str().unwrap(), url.port().unwrap()))
        .await
        .unwrap();
    client.write_all(b"CONNECT unresolvable.private.invalid:443 HTTP/1.1\r\nHost: unresolvable.private.invalid:443\r\n\r\n").await.unwrap();
    let mut header = Vec::new();
    while !header.ends_with(b"\r\n\r\n") {
        header.push(client.read_u8().await.unwrap());
    }
    assert!(header.starts_with(b"HTTP/1.1 200"));
    client.write_all(b"ping").await.unwrap();
    let mut pong = [0; 4];
    client.read_exact(&mut pong).await.unwrap();
    assert_eq!(&pong, b"pong");
    task.await.unwrap();
}
#[tokio::test]
async fn failed_socks_proxy_returns_a_502_instead_of_connecting_to_the_target_directly() {
    let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let unavailable = listener.local_addr().unwrap();
    drop(listener);
    let proxy = Proxy::new(Some(&NetworkProxy::Proxy {
        url: format!("socks5://{unavailable}"),
    }))
    .await
    .unwrap();
    let url = url::Url::parse(&proxy.env["HTTP_PROXY"]).unwrap();
    let mut client = TcpStream::connect((url.host_str().unwrap(), url.port().unwrap()))
        .await
        .unwrap();
    client
        .write_all(b"CONNECT 127.0.0.1:9 HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .await
        .unwrap();
    let mut response = Vec::new();
    client.read_to_end(&mut response).await.unwrap();
    assert!(response.starts_with(b"HTTP/1.1 502"));
    assert!(!String::from_utf8_lossy(&response).contains("socks5"));
}
