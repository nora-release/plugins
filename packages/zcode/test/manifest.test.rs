#[test]
fn packaged_identity_matches_the_executable() {
    let text = include_str!("../nora-plugin.json");
    assert!(text.contains("\"id\": \"zcode\""));
}
