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
        assert_eq!(frame["id"], id, "{frame}");
        assert_ne!(
            frame["event"], "credentials",
            "Reading gifts must not replace an account"
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
async fn sdk_lists_and_spends_personal_trials_while_the_team_window_is_exhausted() {
    for selected in [Site::Zai, Site::BigModel] {
        let token = jwt();
        let credentials = stored(selected, "team.secret", &token);
        let mut replies = vec![
            reply(BALANCE, balance(75)),
            reply(BALANCE, balance(75)),
            preview(),
            message(
                true,
                200,
                json!({"type":"message","content":[{"type":"text","text":"gift answer"}]}),
            ),
            message(
                false,
                429,
                json!({"error":{"message":"team allowance exhausted"}}),
            ),
        ];
        replies.extend(team_usage(100));
        let (plugin, client, store) = configured(replies).await;
        let directory = Directory(
            std::env::temp_dir().join(format!("nora-zcode-team-{}", uuid::Uuid::new_v4())),
        );
        std::fs::create_dir(&directory.0).unwrap();
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
        let initialized=call(&mut input,&mut output,1,"initialize",json!({"apiVersion":1,"pluginID":"zcode","manifest":manifest,"pluginDirectory":directory.0,"dataDirectory":directory.0.join("data"),"subscriptionAccountsEnabled":true})).await;
        assert!(initialized[0].get("result").is_some(), "{initialized:?}");
        let params = json!({"account":"team-account","credentials":credentials,"subscriptionAccountsEnabled":true});
        let models = call(&mut input, &mut output, 2, "models", params.clone()).await;
        assert!(has_trial(&models[0]["result"]));
        let usage = call(&mut input, &mut output, 3, "usage", params.clone()).await;
        assert_eq!(usage[0]["result"]["items"][0]["usedPercent"], 100.0);
        assert_eq!(usage[0]["result"]["items"][0]["notModels"], json!([TRIAL]));
        assert_eq!(usage[0]["result"]["items"][1]["models"], json!([TRIAL]));
        for (id, model, status) in [(4, TRIAL, 200), (5, "GLM-5.3-Flash", 429)] {
            let mut request = params.clone();
            request["method"] = json!("POST");
            request["url"] = json!(format!("{}/v1/messages", selected.base()));
            request["body"]=json!(base64::engine::general_purpose::STANDARD.encode(json!({"model":model,"max_tokens":20,"messages":[{"role":"user","content":"fixture"}]}).to_string()));
            let frames = call(&mut input, &mut output, id, "fetch", request).await;
            assert_eq!(frames[0]["event"], "head");
            assert_eq!(frames[0]["status"], status);
            assert_eq!(frames.last().unwrap()["event"], "done");
            let bytes = frames
                .iter()
                .filter(|frame| frame["event"] == "chunk")
                .flat_map(|frame| {
                    base64::engine::general_purpose::STANDARD
                        .decode(frame["data"].as_str().unwrap())
                        .unwrap()
                })
                .collect::<Vec<_>>();
            let body: Value = serde_json::from_slice(&bytes).unwrap();
            if model == TRIAL {
                assert_eq!(body["content"][0]["text"], "gift answer");
            } else {
                assert_eq!(body["error"]["message"], "team allowance exhausted");
            }
        }
        server.abort();
        let _ = server.await;
        let requests = client.requests.lock().unwrap();
        let messages: Vec<_> = requests
            .iter()
            .filter(|request| request.url.ends_with("/messages"))
            .collect();
        assert_eq!(messages.len(), 2);
        assert_personal(messages[0], &token);
        assert_eq!(messages[1].headers["x-api-key"], "team.secret");
        assert!(client.replies.lock().unwrap().is_empty());
        assert!(store.saved.lock().unwrap().is_empty());
    }
}
