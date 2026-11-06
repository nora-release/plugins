#![allow(dead_code)]
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};
struct Client {
    child: Child,
    reader: BufReader<std::process::ChildStdout>,
    next: u64,
    directory: std::path::PathBuf,
}
impl Client {
    fn new(binary: &str) -> Self {
        let directory = std::env::temp_dir().join(format!(
            "nora-middleware-protocol-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let mut child = Command::new(binary)
            .env_clear()
            .env("HOME", &directory)
            .env("PATH", "/usr/bin:/bin")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let reader = BufReader::new(child.stdout.take().unwrap());
        let mut client = Self {
            child,
            reader,
            next: 1,
            directory,
        };
        let result=client.call("initialize",json!({"apiVersion":2,"pluginDirectory":client.directory,"dataDirectory":client.directory,"manifest":{}}));
        assert_eq!(result["result"]["apiVersion"], 2, "{result}");
        assert_eq!(
            result["result"]["capabilities"]["gatewayMiddleware"]["version"], 1,
            "{result}"
        );
        client
    }
    fn call(&mut self, method: &str, params: Value) -> Value {
        let id = self.next;
        self.next += 1;
        writeln!(
            self.child.stdin.as_mut().unwrap(),
            "{}",
            json!({"id":id,"method":method,"params":params})
        )
        .unwrap();
        self.child.stdin.as_mut().unwrap().flush().unwrap();
        let mut line = String::new();
        assert!(self.reader.read_line(&mut line).unwrap() > 0);
        let value: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(value["id"], id);
        value
    }
    fn phase(&mut self, phase: &str, body: &mut Value, context: &mut Value) -> Value {
        let result = self.call(
            "gatewayMiddleware",
            json!({"version":1,"phase":phase,"body":body,"context":context}),
        );
        assert!(result.get("error").is_none(), "{result}");
        let result = &result["result"];
        context["state"] = result["state"].clone();
        if result["action"] == "replace" {
            *body = result["body"].clone();
        }
        result.clone()
    }
}
impl Drop for Client {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
pub fn official(binary: &str, fixtures: &str) {
    let cases: Value = serde_json::from_str(fixtures).unwrap();
    for c in cases.as_array().unwrap() {
        let mut client = Client::new(binary);
        let mut context = json!({"protocol":c["ctx"]["protocol"].as_str().unwrap_or("anthropic"),"model":c["ctx"]["model"].as_str().unwrap_or(""),"path":c["ctx"]["path"].as_str().unwrap_or(""),"agent":c["ctx"]["agent"].as_str().unwrap_or(""),"stream":c["ctx"]["stream"].as_bool().unwrap_or(false),"status":c["status"].as_u64().unwrap_or(200),"options":c.get("options").cloned().unwrap_or(json!({})),"state":{}});
        if let Some(original) = c.get("request") {
            let mut body = original.clone();
            let result = client.phase("request", &mut body, &mut context);
            if let Some(reject) = c.get("reject") {
                assert_eq!(result["action"], "reject", "{}", c["name"]);
                assert_eq!(result["reject"]["status"], reject["status"]);
                if let Some(text) = reject["message"].as_str() {
                    assert!(result["reject"]["message"].as_str().unwrap().contains(text));
                }
            } else {
                if let Some(provider) = c.get("provider") {
                    context["provider"] = provider.clone();
                    client.phase("provider", &mut body, &mut context);
                }
                assert_eq!(
                    &body,
                    c.get("expect_request").unwrap_or(original),
                    "{}",
                    c["name"]
                );
            }
        }
        if let Some(events) = c["events"].as_array() {
            let mut actual = vec![];
            for event in events {
                let mut value = event["data"].clone();
                if value.is_object() || value.is_array() {
                    if client.phase("event", &mut value, &mut context)["action"] == "drop" {
                        continue;
                    }
                }
                actual.push(value);
            }
            let expected = c.get("expect_events").cloned().unwrap_or_else(|| {
                json!(events.iter().map(|e| e["data"].clone()).collect::<Vec<_>>())
            });
            assert_eq!(json!(actual), expected, "{}", c["name"]);
        }
        if let Some(original) = c.get("response") {
            let mut body = original.clone();
            client.phase("response", &mut body, &mut context);
            assert_eq!(
                &body,
                c.get("expect_response").unwrap_or(original),
                "{}",
                c["name"]
            );
        }
    }
}
pub fn strategies(binary: &str, fixtures: &str) {
    let mut client = Client::new(binary);
    let cases: Value = serde_json::from_str(fixtures).unwrap();
    for c in cases.as_array().unwrap() {
        let result=client.call("gatewayMiddleware",json!({"version":1,"phase":"order","strategy":c["strategy"],"pool":c["pool"].as_str().unwrap_or("pool"),"seats":c["seats"],"options":c.get("options").cloned().unwrap_or(json!({}))}));
        assert!(result.get("error").is_none(), "{result}");
        let keys = result["result"]["order"]
            .as_array()
            .unwrap()
            .iter()
            .map(|i| {
                let i = i.as_u64().unwrap() as usize;
                c["seats"][i].get("key").cloned().unwrap_or(json!(i))
            })
            .collect::<Vec<_>>();
        assert_eq!(json!(keys), c["expect"], "{}", c["name"]);
    }
}
pub fn large_result_and_protocol_validation(binary: &str) {
    let mut client = Client::new(binary);
    let mut body = json!({"model":"old","payload":"x".repeat(600*1024)});
    let context = json!({"options":{"old":"new"},"state":{}});
    let output = client.directory.join("output.json");
    std::fs::write(&output, []).unwrap();
    let reply = client.call(
        "gatewayMiddleware",
        json!({"version":1,"phase":"request","body":body,"context":context,"outputFile":output}),
    );
    assert!(reply.get("error").is_none(), "{reply}");
    assert!(reply["result"].get("body").is_none());
    assert_eq!(
        reply["result"]["bodyFile"]["path"],
        output.to_str().unwrap()
    );
    body["model"] = json!("new");
    assert_eq!(
        serde_json::from_slice::<Value>(&std::fs::read(&output).unwrap()).unwrap(),
        body
    );
    // A result file can only be the fresh empty file offered by the host.
    let rejected=client.call("gatewayMiddleware",json!({"version":1,"phase":"request","body":{"model":"old","payload":"x".repeat(600*1024)},"context":context,"outputFile":output}));
    assert!(rejected.get("error").is_some());
    for request in [
        json!({"version":2,"phase":"request","body":{},"context":{}}),
        json!({"version":1,"phase":"unknown","body":{},"context":{}}),
        json!({"version":1,"phase":"request","body":{},"bodyFile":{},"context":{}}),
    ] {
        assert!(client
            .call("gatewayMiddleware", request)
            .get("error")
            .is_some());
    }
}
