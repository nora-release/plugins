use super::*;
use serde_json::json;
#[test]
fn mcp_auto_drawing_marks_only_images_included_in_subscription_accounts() {
    let definition =
        Definition::parse(serde_json::from_str(include_str!("../nora-plugin.json")).unwrap())
            .unwrap();
    let adapter = OpenAI;
    for credentials in [None, Some(json!({"type":"apiKey","key":"fake"}))] {
        assert!(adapter
            .supplemental_models(&definition, credentials.as_ref())
            .is_empty());
    }
    let credentials =
        json!({"type":"oauth","access":"fake","refresh":"fake","expires":9999999999999u64});
    let models = adapter.supplemental_models(&definition, Some(&credentials));
    assert!(!models.is_empty());
    for model in models {
        if model.info.image_output == Some(true) {
            assert_eq!(model.info.free, Some(true), "{}", model.id);
            assert_eq!(model.info.pricing_known, Some(true), "{}", model.id);
            assert_eq!(model.cost.input + model.cost.output, 0.0);
        } else {
            assert_ne!(model.info.free, Some(true), "{}", model.id);
        }
    }
}
