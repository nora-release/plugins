use super::*;
use nora_provider_plugin_sdk::{NetworkProxy, RequestBodyFile};
use serde_json::json;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct File(PathBuf);
impl File {
    fn new(bytes: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "nora-plugin-upload-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&path, bytes).unwrap();
        Self(path)
    }
}
impl Drop for File {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

fn definition(base: &str) -> Definition {
    Definition::parse(json!({"id":"example","name":"Example","api":"openai-completions","baseURL":base,"config":{"keyProvider":"openai"}})).unwrap()
}
fn request(base: &str, file: &File) -> FetchRequest {
    FetchRequest {
        account: "work".into(),
        credentials: Some(json!({"type":"api","key":"private-upload-key"})),
        network_proxy: Some(NetworkProxy::Direct),
        url: format!("{base}/images/edits"),
        method: "POST".into(),
        headers: BTreeMap::from([
            (
                "content-type".into(),
                "multipart/form-data; boundary=original".into(),
            ),
            ("authorization".into(), "Bearer nora-plugin".into()),
            ("content-length".into(), "1".into()),
        ]),
        metadata: BTreeMap::new(),
        body: None,
        body_file: Some(RequestBodyFile::from_path(&file.0).unwrap()),
    }
}

#[tokio::test]
async fn multipart_files_stream_with_actual_authentication_and_unchanged_binary_bytes() {
    let bytes = b"--original\r\nContent-Disposition: form-data; name=\"image\"; filename=\"a.png\"\r\n\r\n\x00\xff\r\n--original--\r\n";
    let file = File::new(bytes);
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut data = Vec::new();
        let end;
        loop {
            let mut chunk = [0; 4096];
            let count = socket.read(&mut chunk).await.unwrap();
            assert_ne!(count, 0);
            data.extend_from_slice(&chunk[..count]);
            if let Some(index) = data.windows(4).position(|chunk| chunk == b"\r\n\r\n") {
                if data.len() >= index + 4 + bytes.len() {
                    end = index + 4;
                    break;
                }
            }
        }
        let headers = String::from_utf8_lossy(&data[..end]).to_lowercase();
        assert!(headers.contains("authorization: bearer private-upload-key\r\n"));
        assert!(!headers.contains("nora-plugin"));
        assert!(headers.contains(&format!("content-length: {}\r\n", bytes.len())));
        assert_eq!(&data[end..], bytes);
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\n{}")
            .await
            .unwrap();
    });
    let response = fetch(&definition(&base), request(&base, &file))
        .await
        .unwrap();
    assert_eq!(response.status, 200);
    server.await.unwrap();
}

#[tokio::test]
async fn changed_files_foreign_endpoints_and_mixed_bodies_fail_before_upload() {
    let file = File::new(b"original");
    let original = request("http://127.0.0.1:1", &file);
    std::fs::write(&file.0, b"modified").unwrap();
    assert!(fetch(&definition("http://127.0.0.1:1"), original)
        .await
        .is_err());
    let mut request = request("https://foreign.invalid", &file);
    let error = fetch(&definition("https://example.invalid"), request.clone())
        .await
        .err()
        .unwrap();
    assert!(error.message.contains("outside"));
    request.body = Some("eA==".into());
    assert!(fetch(&definition("https://foreign.invalid"), request)
        .await
        .is_err());
}
