use base64::{engine::general_purpose::STANDARD, Engine};
use nora_provider_plugin_sdk::{ProviderPlugin, RequestBodyFile};
use nora_provider_redact::Plugin;
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!(
            "nora-native-privacy-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
    async fn plugin(&self) -> Plugin {
        let p = Plugin::default();
        let result=p.initialize(serde_json::from_value(json!({"apiVersion":1,"pluginID":"privacy","pluginDirectory":self.0,"dataDirectory":self.0,"manifest":{}})).unwrap()).await.unwrap();
        assert_eq!(result["capabilities"]["gatewayMasker"]["version"], 1);
        p
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn call(plugin: &Plugin, value: Value) -> Value {
    plugin.call("gatewayMasker", value).await.unwrap()
}
#[tokio::test]
async fn key_is_persistent_but_original_values_and_previews_are_memory_only() {
    let directory = Directory::new();
    let first = directory.plugin().await;
    let original = "sk-proj-abcdefghijklmnopqrstuvwxyz123456789";
    let p=call(&first,json!({"version":1,"operation":"mask","kind":"text","text":original,"settings":{"redact":true}})).await;
    let placeholder = p["text"].as_str().unwrap();
    assert_ne!(placeholder, original);
    let body = json!({"text":placeholder}).to_string();
    let result=call(&first,json!({"version":1,"operation":"restore","bytes":STANDARD.encode(&body),"finish":true,"contentType":"application/json"})).await;
    assert!(
        String::from_utf8(STANDARD.decode(result["bytes"].as_str().unwrap()).unwrap())
            .unwrap()
            .contains(original)
    );
    let files: Vec<_> = std::fs::read_dir(&directory.0)
        .unwrap()
        .map(|f| f.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1);
    assert_eq!(files[0].file_name().unwrap(), "masking.key");
    assert_eq!(std::fs::read(&files[0]).unwrap().len(), 32);
    let second = directory.plugin().await;
    let absent=call(&second,json!({"version":1,"operation":"restore","bytes":STANDARD.encode(&body),"finish":true,"contentType":"application/json"})).await;
    assert_eq!(
        String::from_utf8(STANDARD.decode(absent["bytes"].as_str().unwrap()).unwrap()).unwrap(),
        body
    );
    let next=call(&second,json!({"version":1,"operation":"mask","kind":"text","text":original,"settings":{"redact":true}})).await;
    assert_eq!(next["text"], placeholder);
    let preview=call(&second,json!({"version":1,"operation":"preview","text":"another.person@masking-fixture.dev","settings":{}})).await;
    assert_eq!(preview["off"], true);
    let value = json!({"text":preview["text"]}).to_string();
    let absent=call(&second,json!({"version":1,"operation":"restore","bytes":STANDARD.encode(&value),"finish":true,"contentType":"application/json"})).await;
    assert_eq!(
        String::from_utf8(STANDARD.decode(absent["bytes"].as_str().unwrap()).unwrap()).unwrap(),
        value
    );
}
#[tokio::test]
async fn large_results_use_the_offered_verified_file_and_reject_changed_inputs() {
    let directory = Directory::new();
    let p = directory.plugin().await;
    let output = directory.0.join("result");
    std::fs::write(&output, []).unwrap();
    let value=call(&p,json!({"version":1,"operation":"mask","kind":"text","text":"privateword ".repeat(25_000),"settings":{"redactWords":["privateword"]},"outputFile":output})).await;
    let file: RequestBodyFile = serde_json::from_value(value["resultFile"].clone()).unwrap();
    assert_eq!(PathBuf::from(&file.path), output);
    let actual: Value = serde_json::from_slice(&file.read_verified().unwrap()).unwrap();
    assert!(!actual["text"].as_str().unwrap().contains("privateword"));
    assert_eq!(actual["hits"].as_array().unwrap().len(), 25_000);
    let input = directory.0.join("input");
    std::fs::write(&input, b"changed").unwrap();
    assert!(p.call("gatewayMasker",json!({"version":1,"payloadFile":{"path":input,"length":7,"sha256":"0".repeat(64)},"outputFile":output})).await.is_err());
}
#[tokio::test]
async fn settings_validation_and_off_preview_do_not_change_later_requests() {
    let directory = Directory::new();
    let p = directory.plugin().await;
    let settings = json!({"redact":false,"redactPersonal":false,"redactRules":[{"kind":"custom","prefix":"relay_"}]});
    let result=call(&p,json!({"version":1,"operation":"mask","kind":"text","text":"relay_abcdefgh123","settings":settings})).await;
    assert_eq!(result["text"], "relay_abcdefgh123");
    for input in [
        json!({"version":0,"operation":"mask"}),
        json!({"version":1,"operation":"settings","settings":{"redactActions":{"email":"warn"}}}),
        json!({"version":1,"operation":"settings","settings":{"redactRules":[{"regex":"a*"}]}}),
    ] {
        assert!(p.call("gatewayMasker", input).await.is_err());
    }
    let output=call(&p,json!({"version":1,"operation":"mask","kind":"text","text":"independentword","settings":{"redactWords":["independentword"]}})).await;
    assert!(output["text"].as_str().unwrap().contains("TERM_"));
}
