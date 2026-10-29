use base64::Engine;
use serde_json::{json, Value};

pub const CLIENT: &str = "b1a00492-073a-47ea-816f-4c329264a828";
pub fn jwt(claims: Value) -> String {
    format!(
        "e30.{}.signature",
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(claims.to_string())
    )
}
pub fn source() -> Value {
    json!({
        "type":"xai", "auth_kind":"oauth", "disabled":false,
        "access_token":jwt(json!({"iss":"https://auth.x.ai","aud":[CLIENT],"sub":"fixture-person"})),
        "refresh_token":"private-cpa-refresh", "sub":"fixture-person", "email":"fixture@example.test",
        "last_refresh":"2026-10-20T00:00:00Z", "expired":"2099-01-01T00:00:00Z",
        "base_url":"https://must-not-use.invalid", "token_type":"Bearer"
    })
}
pub fn params(source: &Value) -> Value {
    json!({"fileName":"cpa.json","format":"auto","content":source.to_string(),"index":0,"credentials":{"type":"pendingImport"}})
}
