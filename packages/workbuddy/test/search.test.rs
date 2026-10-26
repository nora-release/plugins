use super::*;
use crate::tests::{configured, Client};
fn credentials() -> Value {
    json!({"type":"oauth","access":"private-access","refresh":"private-refresh","expires":4102444800000u64,"uid":"member","source":"oauth"})
}
#[tokio::test]
async fn both_workbuddy_regions_search_with_the_current_plan_and_signed_headers() {
    for site in [Site::China, Site::International] {
        let client = Client::new(vec![(
            200,
            json!({"results":[{"title":"Docs","url":"https://example.com","snippet":"Verified facts"}]}),
        )]);
        let provider = configured(site, client.clone());
        let result = provider
            .web_search(
                CallContext {
                    credentials: Some(credentials()),
                    account: "selected-account".into(),
                    ..Default::default()
                },
                "release".into(),
            )
            .await
            .unwrap();
        assert_eq!(result["sources"][0]["url"], "https://example.com");
        let requests = client.requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        let sent = &requests[0];
        assert_eq!(sent.url, format!("{}/agenttool/v1/search", site.endpoint()));
        assert_eq!(sent.headers["authorization"], "Bearer private-access");
        assert_eq!(
            sent.headers["x-request-id"],
            sent.headers["x-conversation-message-id"]
        );
        assert_eq!(sent.headers["x-request-id"].len(), 32);
        assert_eq!(
            serde_json::from_slice::<Value>(sent.body.as_ref().unwrap()).unwrap(),
            json!({"query":"release","type":"text2text","max_results":5})
        );
    }
}
#[tokio::test]
async fn search_quota_refusal_missing_results_and_empty_results_are_errors() {
    for value in [
        json!({"code":15001,"msg":"private-access"}),
        json!({"code":"15001","msg":"private-access"}),
        json!({"results":[]}),
        json!({"wrong":[]}),
    ] {
        let client = Client::new(vec![(200, value)]);
        let provider = configured(Site::China, client);
        let error = provider
            .web_search(
                CallContext {
                    credentials: Some(credentials()),
                    ..Default::default()
                },
                "q".into(),
            )
            .await
            .unwrap_err();
        assert!(!error.to_string().contains("private-access"));
    }
}
#[tokio::test]
async fn compiled_sdk_rpc_advertises_and_dispatches_both_regions_subscription_search() {
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
    for site in [Site::China, Site::International] {
        let client = Client::new(vec![(
            200,
            json!({"results":[{"title":"RPC source","url":"https://example.com/rpc","snippet":"Through the real SDK dispatcher"}]}),
        )]);
        let provider = std::sync::Arc::new(configured(site, client.clone()));
        let manifest: Value = serde_json::from_str(if site == Site::China {
            include_str!("../nora-provider.json")
        } else {
            include_str!("../../workbuddy-ai/nora-provider.json")
        })
        .unwrap();
        let (io, server) = tokio::io::duplex(65536);
        let (server_read, server_write) = tokio::io::split(server);
        let task = tokio::spawn(nora_provider_plugin_sdk::serve_io(
            provider,
            server_read,
            server_write,
        ));
        let (read, mut write) = tokio::io::split(io);
        let mut lines = BufReader::new(read).lines();
        let init = json!({"id":1,"method":"initialize","params":{"apiVersion":1,"pluginID":site.id(),"pluginDirectory":"/tmp/workbuddy-test","dataDirectory":"/tmp/workbuddy-test","manifest":manifest,"subscriptionAccountsEnabled":true}});
        write
            .write_all(format!("{init}\n").as_bytes())
            .await
            .unwrap();
        let initialized: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(
            initialized["result"]["capabilities"]["sdkFeatures"]["webSearch"], 1,
            "{initialized}"
        );
        let search = json!({"id":2,"method":"webSearch","params":{"query":"release","account":"selected","credentials":credentials(),"subscriptionAccountsEnabled":true}});
        write
            .write_all(format!("{search}\n").as_bytes())
            .await
            .unwrap();
        let response: Value =
            serde_json::from_str(&lines.next_line().await.unwrap().unwrap()).unwrap();
        assert_eq!(
            response["result"]["sources"][0]["url"], "https://example.com/rpc",
            "{response}"
        );
        assert_eq!(
            client.requests.lock().unwrap()[0].url,
            format!("{}/agenttool/v1/search", site.endpoint())
        );
        task.abort();
        let _ = task.await;
    }
}
