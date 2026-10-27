use super::*;
#[tokio::test]
async fn compiled_warmup_uses_its_account_and_network_proxy_without_requesting_a_model_catalog() {
    let mut host = Host::new().await;
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let proxy = format!("http://{}", listener.local_addr().unwrap());
    let task = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            bytes.push(socket.read_u8().await.unwrap());
            assert!(bytes.len() < 8192);
        }
        let headers = String::from_utf8(bytes).unwrap();
        assert!(
            headers.starts_with("CONNECT chatgpt.com:443 HTTP/1.1"),
            "{headers}"
        );
        assert!(!headers.contains("fixture-private"));
        socket
            .write_all(
                b"HTTP/1.1 503 Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            )
            .await
            .unwrap();
    });
    let credentials = json!({"type":"oauth","provider":"openAICodex","credentialProvider":"openAICodex","access":"fixture-private","refresh":"fixture-refresh","expires":4070908800000_i64,"extras":{"accountId":"workspace-a"}});
    let response=host.call("warmup",json!({"account":"a","credentials":credentials,"confirmed":true,"models":[{"id":"gpt-luna","reasoningEfforts":["low","high"]}],"networkProxy":{"mode":"proxy","url":proxy}})).await;
    assert!(response.get("error").is_some(), "{response}");
    assert!(!response.to_string().contains("fixture-private"));
    task.await.unwrap();
    let denied=host.call("warmup",json!({"account":"a","credentials":credentials,"confirmed":true,"subscriptionAccountsEnabled":false})).await;
    assert!(denied.get("error").is_some());
    assert!(!host.directory.join("auth.json").exists());
}
