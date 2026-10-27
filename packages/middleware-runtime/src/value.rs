use serde_json::Value;

pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|n| n != 0.),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}
pub fn string(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Object(_) => "[object Object]".into(),
        Value::Array(a) => a
            .iter()
            .map(|v| {
                if v.is_null() {
                    String::new()
                } else {
                    string(v)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        _ => v.to_string(),
    }
}
pub fn text(v: &Value) -> String {
    v.as_str()
        .map(str::to_owned)
        .unwrap_or_else(|| v.to_string())
}
pub fn number(v: &Value) -> Option<f64> {
    match v {
        Value::Null => Some(0.),
        Value::Bool(b) => Some(if *b { 1. } else { 0. }),
        Value::Number(n) => n.as_f64(),
        Value::String(s) => {
            let s = s.trim();
            if s.is_empty() {
                Some(0.)
            } else {
                s.parse().ok()
            }
        }
        Value::Array(_) => number(&Value::String(string(v))),
        _ => None,
    }
}
pub fn array(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or_default()
}
pub fn changed(before: &Value, after: &Value) -> crate::Action {
    if before == after {
        crate::Action::Keep
    } else {
        crate::Action::Replace
    }
}

// ECMAScript Object.keys puts integer-index properties before other keys.
pub fn keys(v: &Value) -> Vec<String> {
    if let Some(a) = v.as_array() {
        return (0..a.len()).map(|i| i.to_string()).collect();
    }
    let Some(o) = v.as_object() else {
        return vec![];
    };
    let mut indexes = vec![];
    let mut rest = vec![];
    for k in o.keys() {
        if let Ok(i) = k.parse::<u32>() {
            if i != u32::MAX && i.to_string() == *k {
                indexes.push((i, k.clone()));
                continue;
            }
        }
        rest.push(k.clone());
    }
    indexes.sort_by_key(|(i, _)| *i);
    indexes.into_iter().map(|(_, k)| k).chain(rest).collect()
}

pub fn trim(text: &str) -> &str {
    text.trim_matches(|c: char| c.is_whitespace() || c == '\u{feff}')
}
