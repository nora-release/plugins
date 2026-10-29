use serde_json::{json, Value};
use std::process::Stdio;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
#[tokio::test]
async fn compiled_plugin_exposes_real_protocol_models_and_cancellable_browser_sign_in_without_vendor_requests(
) {
    let mut child = tokio::process::Command::new(env!("CARGO_BIN_EXE_nora-provider-joycode"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .unwrap();
    let mut input = child.stdin.take().unwrap();
    let mut output = BufReader::new(child.stdout.take().unwrap());
    async fn call(
        input: &mut tokio::process::ChildStdin,
        output: &mut BufReader<tokio::process::ChildStdout>,
        id: u64,
        method: &str,
        params: Value,
    ) -> Value {
        input
            .write_all(format!("{}\n", json!({"id":id,"method":method,"params":params})).as_bytes())
            .await
            .unwrap();
        input.flush().await.unwrap();
        loop {
            let mut line = String::new();
            assert!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    output.read_line(&mut line)
                )
                .await
                .unwrap()
                .unwrap()
                    > 0
            );
            let value: Value = serde_json::from_str(&line).unwrap();
            if value["id"] == id {
                return value;
            }
        }
    }
    let manifest: Value = serde_json::from_str(include_str!("../nora-plugin.json")).unwrap();
    let initialized=call(&mut input,&mut output,1,"initialize",json!({"apiVersion":2,"pluginID":"joycode","pluginDirectory":"/tmp/joycode-cli","dataDirectory":"/tmp/joycode-cli-data","manifest":manifest})).await;
    assert_eq!(initialized["result"]["capabilities"]["persistent"], true);
    let methods = call(&mut input, &mut output, 2, "authMethods", json!({})).await;
    assert_eq!(methods["result"].as_array().unwrap().len(), 2);
    let models = call(&mut input, &mut output, 3, "models", json!({})).await;
    assert_eq!(models["result"]["models"].as_array().unwrap().len(), 5);
    assert_eq!(models["result"]["source"], "preset");
    let invalid = call(
        &mut input,
        &mut output,
        4,
        "fetch",
        json!({"url":"https://foreign.invalid/v1/chat/completions","method":"POST"}),
    )
    .await;
    assert_eq!(invalid["error"]["code"], "invalid_request");
    let flow = call(
        &mut input,
        &mut output,
        5,
        "authorize",
        json!({"method":"oauth"}),
    )
    .await;
    assert!(flow["result"]["url"]
        .as_str()
        .unwrap()
        .starts_with("https://joycode.jd.com/login/"));
    input
        .write_all(
            format!(
                "{}\n",
                json!({"id":6,"method":"callback","params":{"session":flow["result"]["session"]}})
            )
            .as_bytes(),
        )
        .await
        .unwrap();
    input.flush().await.unwrap();
    let aborted = call(&mut input, &mut output, 7, "abort", json!({"id":6})).await;
    assert_eq!(aborted["result"]["aborted"], true);
    child.kill().await.unwrap();
    let url = url::Url::parse(flow["result"]["url"].as_str().unwrap()).unwrap();
    let port = url
        .query_pairs()
        .find(|(key, _)| key == "authPort")
        .unwrap()
        .1
        .parse::<u16>()
        .unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(2), async {
        loop {
            if tokio::net::TcpStream::connect(("127.0.0.1", port))
                .await
                .is_err()
            {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
}
