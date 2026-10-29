use serde_json::Value;
pub fn mutate(v: &mut Value, f: &mut impl FnMut(&str) -> String) {
    if let Some(s) = v.as_str() {
        *v = Value::String(f(s));
    }
}
pub fn parts(v: &mut Value, f: &mut impl FnMut(&str) -> String) {
    if let Some(a) = v.as_array_mut() {
        for p in a {
            if let Some(t) = p.get_mut("text") {
                mutate(t, f);
            }
        }
    }
}
pub fn content(v: &mut Value, f: &mut impl FnMut(&str) -> String) {
    if v.is_string() {
        mutate(v, f);
    } else {
        parts(v, f);
    }
}
pub fn system_message(v: &mut Value, f: &mut impl FnMut(&str) -> String) {
    if matches!(v["role"].as_str(), Some("system" | "developer")) {
        if let Some(c) = v.get_mut("content") {
            content(c, f);
        }
    }
}
