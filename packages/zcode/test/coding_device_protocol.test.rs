use super::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, DuplexStream, ReadHalf, WriteHalf};

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
        assert_eq!(frame["id"], id, "{frame}");
        assert_ne!(
            frame["event"], "credentials",
            "A coding request must not replace a credential"
        );
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
async fn sdk_initialize_and_fetch_mark_messages_only_for_each_site() {
    for selected in [Site::Zai, Site::BigModel] {
        let (mut plugin, client, store) = setup(vec![
            message("/v1/messages", 200),
            message("/v1/messages/count_tokens", 200),
        ]);
        plugin.core = Core::with_dependencies("zcode", client.clone(), store.clone());
        let directory = SourceDirectory::new();
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
        let initialized = call(&mut input, &mut output, 1, "initialize", json!({"apiVersion":1,"pluginID":"zcode","manifest":manifest,"pluginDirectory":directory.0,"dataDirectory":directory.0.join("data"),"subscriptionAccountsEnabled":true})).await;
        assert!(initialized[0].get("result").is_some(), "{initialized:?}");
        let mut stored = saved(selected);
        stored["type"] = json!("api");
        stored["key"] = json!("id.pasted");
        stored["state"].as_object_mut().unwrap().remove("device");
        let original = serde_json::to_vec(&pi()).unwrap();
        for (id, path) in [(2, "/v1/messages"), (3, "/v1/messages/count_tokens")] {
            let frames = call(&mut input, &mut output, id, "fetch", json!({"account":"fixture","credentials":stored,"subscriptionAccountsEnabled":true,"method":"POST","url":format!("{}{path}",selected.base()),"headers":{"content-type":"application/json"},"body":base64::engine::general_purpose::STANDARD.encode(&original)})).await;
            assert_eq!(frames[0]["event"], "head", "{frames:?}");
            assert_eq!(frames[0]["status"], 200);
            assert_eq!(frames.last().unwrap()["event"], "done");
            let response = frames
                .iter()
                .filter(|frame| frame["event"] == "chunk")
                .flat_map(|frame| {
                    base64::engine::general_purpose::STANDARD
                        .decode(frame["data"].as_str().unwrap())
                        .unwrap()
                })
                .collect::<Vec<_>>();
            assert_eq!(
                serde_json::from_slice::<Value>(&response).unwrap(),
                json!({"type":"message","content":[]})
            );
        }
        server.abort();
        let _ = server.await;
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 2);
        assert_eq!(
            body(&requests[0])["metadata"]["user_id"],
            user_id(PASTED_DEVICE)
        );
        assert_eq!(requests[1].body.as_ref().unwrap(), &original);
        assert!(client.replies.lock().unwrap().is_empty());
        assert!(store.saved.lock().unwrap().is_empty());
    }
}
