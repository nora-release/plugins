use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

struct Directory(PathBuf);
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn call(
    reader: &mut BufReader<ReadHalf<DuplexStream>>,
    writer: &mut WriteHalf<DuplexStream>,
    id: u64,
    method: &str,
    params: Value,
) -> Vec<Value> {
    writer
        .write_all(format!("{}\n", json!({"id":id,"method":method,"params":params})).as_bytes())
        .await
        .unwrap();
    let mut frames = Vec::new();
    loop {
        let mut line = String::new();
        assert!(
            tokio::time::timeout(Duration::from_secs(5), reader.read_line(&mut line))
                .await
                .unwrap()
                .unwrap()
                > 0
        );
        let frame: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(frame["id"], id);
        assert_ne!(frame["event"], "credentials");
        let done = frame.get("result").is_some()
            || frame.get("error").is_some()
            || frame["event"] == "done";
        frames.push(frame);
        if done {
            return frames;
        }
    }
}

#[tokio::test]
async fn sdk_outputs_original_tool_names_for_json_and_sse_and_keeps_stream_failures() {
    let encoded = sse(&events());
    let mut broken = response(&encoded, "text/event-stream", 7);
    broken.chunks.push(Err("fixture stream failed"));
    let (mut plugin, client, store) = configured(vec![
        response(
            &serde_json::to_vec(&tool_answer()).unwrap(),
            "application/json",
            3,
        ),
        response(&encoded, "text/event-stream", 7),
        broken,
    ]);
    plugin.core = Core::with_dependencies("zcode", client.clone(), store.clone());
    let dir =
        Directory(std::env::temp_dir().join(format!("nora-zcode-tools-{}", uuid::Uuid::new_v4())));
    std::fs::create_dir(&dir.0).unwrap();
    let (host, guest) = tokio::io::duplex(64 * 1024);
    let (input, output) = tokio::io::split(guest);
    let server = tokio::spawn(nora_provider_plugin_sdk::serve_io(
        Arc::new(plugin),
        input,
        output,
    ));
    let (input, mut output) = tokio::io::split(host);
    let mut input = BufReader::new(input);
    let manifest: Value = serde_json::from_str(include_str!("../nora-provider.json")).unwrap();
    let init=call(&mut input,&mut output,1,"initialize",json!({"apiVersion":1,"pluginID":"zcode","manifest":manifest,"pluginDirectory":dir.0,"dataDirectory":dir.0.join("data"),"subscriptionAccountsEnabled":true})).await;
    assert!(init[0].get("result").is_some());
    for id in 2..=4 {
        let frames=call(&mut input,&mut output,id,"fetch",json!({"account":"fixture","credentials":credentials("fixture.key",""),"subscriptionAccountsEnabled":true,"method":"POST","url":format!("{}/v1/messages",Site::Zai.base()),"body":base64::engine::general_purpose::STANDARD.encode(serde_json::to_vec(&claude_code()).unwrap())})).await;
        assert_eq!(frames[0]["event"], "head");
        assert_eq!(frames[0]["status"], 200);
        let out = frames
            .iter()
            .filter(|frame| frame["event"] == "chunk")
            .flat_map(|frame| {
                base64::engine::general_purpose::STANDARD
                    .decode(frame["data"].as_str().unwrap())
                    .unwrap()
            })
            .collect::<Vec<_>>();
        if id == 2 {
            assert_eq!(
                serde_json::from_slice::<Value>(&out).unwrap(),
                restored_answer()
            );
        } else {
            assert_eq!(read_events(&out)[2]["content_block"]["name"], "ToolSearch");
        }
        if id == 4 {
            assert!(frames.last().unwrap().get("error").is_some());
            assert!(frames.iter().all(|frame| frame["event"] != "done"));
        } else {
            assert_eq!(frames.last().unwrap()["event"], "done");
        }
    }
    server.abort();
    let _ = server.await;
    assert_eq!(client.requests.lock().unwrap().len(), 3);
    for sent in client.requests.lock().unwrap().iter() {
        assert_searchless(&serde_json::from_slice(sent.body.as_ref().unwrap()).unwrap());
    }
    assert!(store.saved.lock().unwrap().is_empty());
}
