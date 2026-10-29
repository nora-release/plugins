#[path = "../../middleware-runtime/test/protocol.rs"]
mod protocol;
#[test]
fn compiled_plugin_official_cases() {
    protocol::official(
        env!("CARGO_BIN_EXE_nora-provider-block-patterns"),
        include_str!("fixtures/cases.json"),
    );
}
