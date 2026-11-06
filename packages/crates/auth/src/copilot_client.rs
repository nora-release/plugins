use std::collections::BTreeMap;

pub fn editor_headers() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("editor-version".into(), "vscode/1.140.0".into()),
        ("editor-plugin-version".into(), "copilot-chat/0.68.0".into()),
        ("user-agent".into(), "GitHubCopilotChat/0.68.0".into()),
        ("copilot-integration-id".into(), "vscode-chat".into()),
    ])
}

pub fn api_headers() -> BTreeMap<String, String> {
    let mut headers = editor_headers();
    headers.insert("x-github-api-version".into(), "2026-01-09".into());
    headers
}

pub fn cli_headers() -> BTreeMap<String, String> {
    BTreeMap::from([
        ("editor-version".into(), "copilot/1.0.88".into()),
        (
            "copilot-integration-id".into(),
            "copilot-developer-cli".into(),
        ),
        ("x-github-api-version".into(), "2026-07-01".into()),
        ("user-agent".into(), "copilot/1.0.88".into()),
    ])
}
