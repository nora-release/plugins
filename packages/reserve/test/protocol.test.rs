#[path = "../../middleware-runtime/test/protocol.rs"]
mod protocol;
#[test]
fn compiled_plugin_official_cases() {
    protocol::strategies(
        env!("CARGO_BIN_EXE_nora-provider-reserve"),
        include_str!("fixtures/strategies.json"),
    );
}
