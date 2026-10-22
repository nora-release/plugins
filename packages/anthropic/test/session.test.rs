use super::*;
#[tokio::test]
async fn session_reuse_changes_when_proxy_or_prompt_ownership_changes() {
    let proxy = Proxy::new(None).await.unwrap();
    let request = json!({"model":"claude-test","system":"one","messages":[],"tools":[]});
    let mut changed = request.clone();
    changed["system"] = json!("two");
    assert_ne!(signature(&request, &proxy), signature(&changed, &proxy));
    let other = Proxy::new(Some(&nora_provider_plugin_sdk::NetworkProxy::Proxy {
        url: "http://127.0.0.1:8080".into(),
    }))
    .await
    .unwrap();
    assert_ne!(signature(&request, &proxy), signature(&request, &other));
}
