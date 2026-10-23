#[test]
fn packaged_identity_matches_the_executable() {
    let text = include_str!("../nora-provider.json");
    assert!(text.contains("\"id\": \"minimax-code-global\""));
    assert!(text.contains("\"type\": \"native\""));
}
