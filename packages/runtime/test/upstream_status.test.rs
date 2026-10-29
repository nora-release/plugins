use super::*;
use serde_json::json;

#[test]
fn vendor_owned_status_declarations_match_the_reference_components_and_hosts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let expected = [
        (
            "openai",
            json!({"vendor":"openai","name":"OpenAI","page":"https://status.openai.com","summary":"https://status.openai.com/api/v2/summary.json","hosts":["api.openai.com","chatgpt.com"],"components":["Chat Completions","Responses","Codex","Codex CLI"]}),
        ),
        (
            "anthropic",
            json!({"vendor":"anthropic","name":"Anthropic","page":"https://status.claude.com","summary":"https://status.claude.com/api/v2/summary.json","hosts":["api.anthropic.com"],"components":["Claude Code"],"componentPrefixes":["Claude API"]}),
        ),
        (
            "moonshotai",
            json!({"vendor":"moonshot","name":"Moonshot AI","page":"https://status.moonshot.cn","summary":"https://status.moonshot.cn/api/v2/summary.json","hosts":["api.moonshot.cn","api.moonshot.ai"],"components":["Open API","API Service","Model"],"componentSuffixes":[" Model"]}),
        ),
        (
            "deepseek",
            json!({"vendor":"deepseek","name":"DeepSeek","page":"https://status.deepseek.com","hosts":["api.deepseek.com"]}),
        ),
    ];
    for (id, status) in expected {
        let manifest: Value =
            serde_json::from_slice(&std::fs::read(root.join(id).join("nora-plugin.json")).unwrap())
                .unwrap();
        assert_eq!(
            manifest["contributes"]["provider"]["config"]["upstreamStatus"], status,
            "{id}"
        );
        for host in status["hosts"].as_array().unwrap() {
            assert!(!host.as_str().unwrap().contains('/'));
            assert!(!host.as_str().unwrap().contains("relay"));
        }
        let temp =
            std::env::temp_dir().join(format!("nora-status-prepare-{}", uuid::Uuid::new_v4()));
        prepare(&root.join(id).join("nora-plugin.json"), &temp).unwrap();
        let prepared: Value =
            serde_json::from_slice(&std::fs::read(temp.join(id).join("nora-plugin.json")).unwrap())
                .unwrap();
        assert_eq!(
            prepared["contributes"]["provider"]["config"]["upstreamStatus"],
            status
        );
        std::fs::remove_dir_all(temp).unwrap();
    }
    let cn: Value = serde_json::from_slice(
        &std::fs::read(root.join("moonshotai-cn/nora-plugin.json")).unwrap(),
    )
    .unwrap();
    let global: Value =
        serde_json::from_slice(&std::fs::read(root.join("moonshotai/nora-plugin.json")).unwrap())
            .unwrap();
    assert_eq!(
        cn["config"]["upstreamStatus"],
        global["config"]["upstreamStatus"]
    );
}
