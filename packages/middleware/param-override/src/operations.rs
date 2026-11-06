use crate::{conditions, paths};
use nora_gateway_middleware::{
    regex::Expression,
    value::{keys, number, string, text, truthy},
    Action,
};
use serde_json::{json, Value};

pub fn run(root: &mut Value, op: &Value, req: &Value) -> Result<Option<Action>, String> {
    let mode = op["mode"].as_str().unwrap_or("");
    if mode == "return_error" {
        return Ok(Some(error(&op["value"])));
    }
    if matches!(mode, "move" | "copy") {
        let from = paths::resolve(root, &paths::split(&op["from"]));
        let to = paths::resolve(root, &paths::split(&op["to"]));
        if from.len() != 1 || to.len() != 1 {
            return Ok(None);
        }
        if let Some(value) = paths::get(root, &from[0]).cloned() {
            paths::set(root, &to[0], value);
            if mode == "move" {
                paths::remove(root, &from[0]);
            }
        }
        return Ok(None);
    }
    let mut ps = if mode == "prune_objects" && !truthy(&op["path"]) {
        vec![vec![]]
    } else {
        paths::resolve(root, &paths::split(&op["path"]))
    };
    if mode == "delete" {
        ps.reverse();
    }
    for p in ps {
        let current = paths::get(root, &p);
        let value = &op["value"];
        let next = match mode {
            "set" => {
                if truthy(&op["keep_origin"]) && current.is_some() {
                    continue;
                }
                if op.get("value").is_none() {
                    paths::remove(root, &p);
                    continue;
                }
                value.clone()
            }
            "delete" => {
                paths::remove(root, &p);
                continue;
            }
            "append" | "prepend" => {
                let Some(next) = current.and_then(|cur| {
                    join(cur, value, mode == "prepend", truthy(&op["keep_origin"]))
                }) else {
                    continue;
                };
                next
            }
            "trim_prefix" | "trim_suffix" | "ensure_prefix" | "ensure_suffix" => {
                let Some(cur) = current.and_then(Value::as_str) else {
                    continue;
                };
                if value.is_null() {
                    continue;
                }
                let v = string(value);
                json!(match mode {
                    "trim_prefix" => cur.strip_prefix(&v).unwrap_or(cur).to_string(),
                    "trim_suffix" => cur.strip_suffix(&v).unwrap_or(cur).to_string(),
                    "ensure_prefix" =>
                        if cur.starts_with(&v) {
                            cur.into()
                        } else {
                            format!("{v}{cur}")
                        },
                    _ =>
                        if cur.ends_with(&v) {
                            cur.into()
                        } else {
                            format!("{cur}{v}")
                        },
                })
            }
            "trim_space" | "to_lower" | "to_upper" => {
                let Some(cur) = current.and_then(Value::as_str) else {
                    continue;
                };
                json!(match mode {
                    "trim_space" => cur.trim().to_string(),
                    "to_lower" => cur.to_lowercase(),
                    _ => cur.to_uppercase(),
                })
            }
            "replace" => {
                let Some(cur) = current.and_then(Value::as_str) else {
                    continue;
                };
                if !truthy(&op["from"]) {
                    continue;
                }
                let from = string(&op["from"]);
                let to = op
                    .get("to")
                    .filter(|v| !v.is_null())
                    .map(string)
                    .unwrap_or_default();
                json!(cur.replace(&from, &to))
            }
            "regex_replace" => {
                let Some(cur) = current.and_then(Value::as_str) else {
                    continue;
                };
                if !truthy(&op["from"]) {
                    continue;
                }
                let to = op
                    .get("to")
                    .filter(|v| !v.is_null())
                    .map(string)
                    .unwrap_or_default();
                json!(go_regex(&string(&op["from"]))?.replace(cur, &go_replacement(&to), true))
            }
            "prune_objects" => {
                let Some(current) = current else { continue };
                prune(current, value, req)
            }
            _ => return Ok(None),
        };
        paths::set(root, &p, next);
    }
    Ok(None)
}
fn error(value: &Value) -> Action {
    let mut status = 400.;
    let mut message = String::new();
    if let Some(s) = value.as_str() {
        message = s.trim().into();
    } else if value.is_object() {
        message = value
            .get("message")
            .filter(|v| !v.is_null())
            .or_else(|| value.get("msg").filter(|v| !v.is_null()))
            .map(string)
            .unwrap_or_default()
            .trim()
            .into();
        if let Some(s) = value
            .get("status_code")
            .filter(|v| !v.is_null())
            .or_else(|| value.get("status"))
            .and_then(number)
            .filter(|s| s.is_finite() && s.fract() == 0.)
        {
            status = s;
        }
    }
    if message.is_empty() {
        message = "turned away by param-override".into();
    }
    Action::Reject {
        status: if (400. ..600.).contains(&status) {
            status as u16
        } else {
            403
        },
        message,
    }
}
fn join(cur: &Value, value: &Value, before: bool, keep: bool) -> Option<Value> {
    if let Some(a) = cur.as_array() {
        let add = value
            .as_array()
            .cloned()
            .unwrap_or_else(|| vec![value.clone()]);
        return Some(json!(if before {
            add.into_iter().chain(a.iter().cloned()).collect::<Vec<_>>()
        } else {
            a.iter().cloned().chain(add).collect::<Vec<_>>()
        }));
    }
    if let Some(s) = cur.as_str() {
        let v = text(value);
        return Some(json!(if before {
            format!("{v}{s}")
        } else {
            format!("{s}{v}")
        }));
    }
    if cur.is_object() && value.is_object() {
        let mut out = cur.clone();
        for k in keys(value) {
            if !keep || out.get(&k).is_none() {
                out[&k] = value[&k].clone();
            }
        }
        return Some(out);
    }
    None
}
fn prune(node: &Value, rule: &Value, req: &Value) -> Value {
    let mut cs = vec![];
    let mut logic = "AND";
    let mut recursive = true;
    if let Some(s) = rule.as_str().map(str::trim).filter(|s| !s.is_empty()) {
        cs.push(json!({"path":"type","mode":"full","value":s}));
    } else if rule.is_object() {
        logic = rule["logic"]
            .as_str()
            .filter(|s| !s.trim().is_empty())
            .unwrap_or("AND");
        recursive = rule["recursive"].as_bool().unwrap_or(true);
        cs.extend(conditions::list(&rule["conditions"]));
        if rule["where"].is_object() {
            cs.extend(conditions::list(&rule["where"]));
        }
        if let Some(t) = rule.get("type") {
            cs.push(json!({"path":"type","mode":"full","value":t}));
        }
    }
    if cs.is_empty() {
        return node.clone();
    }
    prune_inner(node, &json!(cs), logic, recursive, req)
}
fn prune_inner(node: &Value, cs: &Value, logic: &str, recursive: bool, req: &Value) -> Value {
    let keep = |v: &Value| !v.is_object() || !conditions::when(v, req, cs, logic);
    let deeper = |v: &Value| {
        if recursive && (v.is_object() || v.is_array()) {
            prune_inner(v, cs, logic, recursive, req)
        } else {
            v.clone()
        }
    };
    match node {
        Value::Array(a) => json!(a.iter().filter(|v| keep(v)).map(deeper).collect::<Vec<_>>()),
        Value::Object(o) => Value::Object(
            o.iter()
                .filter(|(_, v)| keep(v))
                .map(|(k, v)| (k.clone(), deeper(v)))
                .collect(),
        ),
        _ => node.clone(),
    }
}
fn go_regex(pattern: &str) -> Result<Expression, String> {
    let mut pattern = pattern;
    let mut flags = String::from("g");
    if let Some(rest) = pattern.strip_prefix("(?") {
        if let Some((f, p)) = rest.split_once(')') {
            if !f.is_empty() && f.chars().all(|c| "imsU".contains(c)) {
                pattern = p;
                for c in f.chars() {
                    if c != 'U' && !flags.contains(c) {
                        flags.push(c);
                    }
                }
            }
        }
    }
    Expression::new(&pattern.replace("(?P<", "(?<"), &flags)
}
fn go_replacement(input: &str) -> String {
    let mut out = String::new();
    let mut remaining = input;
    while let Some(start) = remaining.find("${") {
        out.push_str(&remaining[..start]);
        remaining = &remaining[start + 2..];
        let Some(end) = remaining.find('}') else {
            out.push_str("${");
            out.push_str(remaining);
            return out;
        };
        let name = &remaining[..end];
        if !name.is_empty() && name.chars().all(|c| c.is_ascii_digit()) {
            out.push('$');
            out.push_str(name);
            remaining = &remaining[end + 1..];
        } else if name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        {
            out.push_str("$<");
            out.push_str(name);
            out.push('>');
            remaining = &remaining[end + 1..];
        } else {
            out.push_str("${");
        }
    }
    out.push_str(remaining);
    out
}
