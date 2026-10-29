use base64::{engine::general_purpose::STANDARD, Engine};
use nora_provider_plugin_sdk::{serve_io, ProviderPlugin};
use serde_json::{json, Value};
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub async fn fetch(
    provider: impl ProviderPlugin + 'static,
    manifest: Value,
    credentials: Value,
    url: String,
    chat: Value,
) -> (u16, Vec<Value>) {
    let directory =
        Directory(std::env::temp_dir().join(format!("nora-xml-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir_all(&directory.0).unwrap();
    let (host, guest) = tokio::io::duplex(64 * 1024);
    let (input, output) = tokio::io::split(guest);
    let server = tokio::spawn(serve_io(Arc::new(provider), input, output));
    let (reader, mut writer) = tokio::io::split(host);
    let mut reader = BufReader::new(reader);
    let run = async {
        let init = json!({"id":1,"method":"initialize","params":{
            "apiVersion":2,"pluginID":manifest["id"],"manifest":manifest,
            "pluginDirectory":directory.0,"dataDirectory":directory.0,
            "subscriptionAccountsEnabled":true
        }});
        writer
            .write_all(format!("{init}\n").as_bytes())
            .await
            .unwrap();
        let mut line = String::new();
        assert!(reader.read_line(&mut line).await.unwrap() > 0);
        let initialized: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(initialized["id"], 1);
        assert!(initialized.get("result").is_some(), "{initialized}");
        let request = json!({"id":2,"method":"fetch","params":{
            "account":"fixture","credentials":credentials,"method":"POST","url":url,
            "body":STANDARD.encode(chat.to_string())
        }});
        writer
            .write_all(format!("{request}\n").as_bytes())
            .await
            .unwrap();
        let mut status = None;
        let mut bytes = Vec::new();
        loop {
            line.clear();
            assert!(reader.read_line(&mut line).await.unwrap() > 0);
            let frame: Value = serde_json::from_str(&line).unwrap();
            assert_eq!(frame["id"], 2, "{frame}");
            match frame["event"].as_str() {
                Some("head") => {
                    assert!(status.is_none());
                    status = frame["status"].as_u64();
                }
                Some("chunk") => {
                    assert!(status.is_some());
                    bytes.extend(STANDARD.decode(frame["data"].as_str().unwrap()).unwrap());
                }
                Some("done") => break,
                _ => panic!("Unexpected protocol frame: {frame}"),
            }
        }
        let text = String::from_utf8(bytes).unwrap();
        let chunks: Vec<Value> = text
            .lines()
            .filter_map(|line| line.strip_prefix("data: "))
            .filter(|data| *data != "[DONE]")
            .map(|data| serde_json::from_str(data).unwrap())
            .collect();
        if !chunks.iter().any(|chunk| chunk.get("error").is_some()) {
            assert!(
                text.ends_with("data: [DONE]\n\n"),
                "status {status:?}: {text}"
            );
        }
        (status.unwrap() as u16, chunks)
    };
    let result = tokio::time::timeout(Duration::from_secs(5), run).await;
    server.abort();
    let _ = server.await;
    result.unwrap()
}

pub fn assert_calls(chunks: &[Value], expected: &[Value], error: bool) {
    let calls: Vec<Value> = chunks
        .iter()
        .flat_map(|chunk| {
            chunk["choices"][0]["delta"]["tool_calls"]
                .as_array()
                .into_iter()
                .flatten()
        })
        .map(|call| {
            json!([
                call["function"]["name"],
                serde_json::from_str::<Value>(call["function"]["arguments"].as_str().unwrap())
                    .unwrap()
            ])
        })
        .collect();
    assert_eq!(calls, expected);
    assert!(chunks
        .iter()
        .all(|chunk| chunk["choices"][0]["delta"]["content"]
            .as_str()
            .unwrap_or("")
            .is_empty()));
    let finish: Vec<_> = chunks
        .iter()
        .filter_map(|chunk| chunk["choices"][0]["finish_reason"].as_str())
        .collect();
    assert_eq!(finish, if error { vec![] } else { vec!["tool_calls"] });
    assert_eq!(
        chunks.iter().any(|chunk| chunk["error"]["message"]
            .as_str()
            .is_some_and(|s| s.contains("cut short"))),
        error
    );
}
