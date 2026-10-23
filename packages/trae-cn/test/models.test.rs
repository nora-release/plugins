use super::*;
use crate::tests::{configured, credentials, listing, Client};
#[test]
fn models_choose_the_live_function_and_name_and_offer_only_a_real_larger_max_mode() {
    let lists = BTreeMap::from([
        (
            "chat_v3".into(),
            vec![
                json!({"config_name":"flash-4.1","display_name":"Flash 4","model_detail_list":[{"model_name":"flash__dev","max_tokens":32000},{"model_name":"flash__max","max_tokens":64000}],"context_window_tokens":{"dev":200000,"max":1000000}}),
                json!({"config_name":"flash-4","display_name":"Flash 4"}),
            ],
        ),
        (
            "solo_agent".into(),
            vec![
                json!({"config_name":"flash-4.1","display_name":"Flash 4.1","model_detail_list":[{"model_name":"flash-4.1__dev","max_tokens":128000}],"context_window_tokens":{"dev":200000}}),
            ],
        ),
        (
            "solo_agent_lite".into(),
            vec![
                json!({"config_name":"flash-4.1","display_name":"Flash 4.1","model_detail_list":[{"model_name":"flash-4.1__dev","max_tokens":128000}]}),
            ],
        ),
    ]);
    let entries = merge(Site::China, &lists).unwrap();
    let normal = entries
        .iter()
        .find(|entry| entry.id == "flash-4.1")
        .unwrap();
    assert_eq!(normal.name, "Flash 4.1");
    assert_eq!(normal.function, "solo_agent");
    assert_eq!(normal.model_name, "flash-4.1__dev");
    let max = entries
        .iter()
        .find(|entry| entry.id == "flash-4.1-max")
        .unwrap();
    assert_eq!(max.function, "chat_v3");
    assert_eq!(max.context, 1000000);
    assert_eq!(max.model_name, "flash__max");
    assert_eq!(max.max, Some(("flash-4.1".into(), 1000000)));
}
#[test]
fn helper_custom_disabled_and_global_invisible_configs_are_not_exposed_as_models() {
    let values = vec![
        json!({"config_name":"real","multimodal":false,"prompt_max_tokens":32000}),
        json!({"config_name":"summary","usage":"summary"}),
        json!({"config_name":"custom_model_1"}),
        json!({"config_name":"switched-off","config_switch":false}),
        json!({"config_name":"hidden","is_invisible_to_user":true}),
    ];
    let lists = BTreeMap::from([("chat_v3".into(), values)]);
    let global = merge(Site::Global, &lists).unwrap();
    assert_eq!(global.len(), 1);
    assert_eq!(global[0].context, 32000);
    assert!(!global[0].sees);
    assert_eq!(global[0].most, 0);
    assert_eq!(merge(Site::China, &lists).unwrap().len(), 2);
    assert!(merge(Site::Global, &BTreeMap::new()).is_err());
}
#[tokio::test]
async fn batch_catalog_is_account_live_data_and_reused_by_the_first_chat() {
    let client = Client::new(vec![(200, listing())]);
    let provider = configured(Site::China, client.clone());
    let credentials = credentials();
    let a = fetch(&provider, client.as_ref(), &credentials, true)
        .await
        .unwrap();
    let b = fetch(&provider, client.as_ref(), &credentials, false)
        .await
        .unwrap();
    assert_eq!(a[0].id, b[0].id);
    let requests = client.requests.lock().unwrap();
    assert_eq!(requests.len(), 1);
    assert!(requests[0]
        .url
        .ends_with("/api/ide/v1/batch_get_detail_param"));
    let body: Value = serde_json::from_slice(requests[0].body.as_ref().unwrap()).unwrap();
    assert_eq!(body["functions"], json!(site::FUNCTIONS));
}
