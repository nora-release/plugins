#[path = "../../middleware-runtime/test/protocol.rs"]
mod protocol;
#[test]
fn compiled_plugin_official_cases() {
    protocol::official(
        env!("CARGO_BIN_EXE_nora-provider-model-map"),
        include_str!("fixtures/cases.json"),
    );
}
#[test]
fn compiled_plugin_large_results_and_validation() {
    protocol::large_result_and_protocol_validation(env!("CARGO_BIN_EXE_nora-provider-model-map"));
}
