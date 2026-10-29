use nora_provider_redact::{Engine, Settings};
use serde::Deserialize;
#[derive(Deserialize)]
struct Case {
    source: String,
    input: String,
    output: String,
    mode: u8,
    json: bool,
}
#[test]
fn frozen_magpie_text_and_json_fixtures() {
    let cases: Vec<Case> = serde_json::from_str(include_str!("fixtures/magpie.json")).unwrap();
    let mut failed = Vec::new();
    for (i, c) in cases.iter().enumerate() {
        let mut engine = Engine::new([7; 32]);
        let mut settings = Settings {
            redact: c.mode != 1,
            redact_personal: c.mode != 0,
            redact_shape: c.mode == 3,
            ..Default::default()
        };
        if c.mode >= 2 {
            for kind in [
                "ssn", "passport", "iban", "birthday", "mac", "serial", "home", "userhost", "ip",
                "bucket",
            ] {
                settings.redact_kinds.insert(kind.into(), true);
            }
        }
        let actual = engine.mask(&c.input, &settings, c.json);
        if actual != c.output {
            failed.push(serde_json::json!({"index":i,"source":c.source,"input":c.input,"mode":c.mode,"expected":c.output,"actual":actual}));
        }
    }
    if !failed.is_empty() {
        std::fs::write(
            "/tmp/nora-core023-plugin-differences.json",
            serde_json::to_vec_pretty(&failed).unwrap(),
        )
        .unwrap();
    }
    assert!(
        failed.is_empty(),
        "{} of {} frozen Magpie cases differ; first: {:?}",
        failed.len(),
        cases.len(),
        failed.first()
    );
}
