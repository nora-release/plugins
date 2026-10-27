#[path = "../../middleware-runtime/test/official.rs"]
mod official;

#[test]
fn official_cases() {
    official::run::<nora_provider_think_tags::Plugin>(include_str!("fixtures/cases.json"));
}
