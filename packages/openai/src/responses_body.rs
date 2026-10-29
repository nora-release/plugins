use serde_json::{json, Value};
use url::Url;

pub(crate) fn finalize(subscription: bool, target: &Url, body: &mut Option<Vec<u8>>) {
    if !subscription
        || target.host_str() != Some("chatgpt.com")
        || target.path() != "/backend-api/codex/responses"
    {
        return;
    }
    let Some(bytes) = body.as_ref() else { return };
    let Ok(mut value) = serde_json::from_slice::<Value>(bytes) else {
        return;
    };
    let Some(object) = value.as_object_mut() else {
        return;
    };
    super::fast::apply(object);
    object.insert("store".into(), json!(false));
    object.insert("stream".into(), json!(true));
    if object
        .get("reasoning")
        .and_then(|value| value.get("effort"))
        .and_then(Value::as_str)
        == Some("none")
    {
        object.remove("include");
    } else {
        let mut include = object
            .get("include")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        if !include
            .iter()
            .any(|value| value == "reasoning.encrypted_content")
        {
            include.push(json!("reasoning.encrypted_content"));
        }
        object.insert("include".into(), Value::Array(include));
    }
    if let Ok(bytes) = serde_json::to_vec(&value) {
        *body = Some(bytes);
    }
}
