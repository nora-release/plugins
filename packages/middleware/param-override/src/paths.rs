use nora_gateway_middleware::value::string;
use serde_json::{json, Value};
pub fn split(value: &Value) -> Vec<String> {
    let path = if value.is_null() {
        String::new()
    } else {
        string(value)
    };
    if path.is_empty() {
        return vec![];
    }
    let mut out = vec![];
    let mut current = String::new();
    let mut chars = path.chars();
    while let Some(c) = chars.next() {
        match c {
            '\\' => {
                current.push(chars.next().unwrap_or('\\'));
            }
            '.' => {
                out.push(std::mem::take(&mut current));
            }
            _ => current.push(c),
        }
    }
    out.push(current);
    out
}
pub fn get<'a>(root: &'a Value, path: &[String]) -> Option<&'a Value> {
    let mut node = root;
    for k in path {
        node = match node {
            Value::Array(a) => a.get(k.parse::<usize>().ok()?),
            Value::Object(o) => o.get(k),
            _ => None,
        }?;
    }
    Some(node)
}
pub fn resolve(root: &Value, keys: &[String]) -> Vec<Vec<String>> {
    let mut paths = vec![vec![]];
    for key in keys {
        let mut next = vec![];
        for path in paths {
            let node = get(root, &path);
            if key == "*" {
                match node {
                    Some(Value::Array(a)) => {
                        for i in 0..a.len() {
                            let mut p = path.clone();
                            p.push(i.to_string());
                            next.push(p);
                        }
                    }
                    Some(Value::Object(o)) => {
                        let mut keys: Vec<_> = o.keys().cloned().collect();
                        keys.sort();
                        for k in keys {
                            let mut p = path.clone();
                            p.push(k);
                            next.push(p);
                        }
                    }
                    _ => {}
                }
            } else if let Some(Value::Array(a)) = node {
                let valid = key.strip_prefix('-').unwrap_or(key);
                if valid.is_empty() || !valid.chars().all(|c| c.is_ascii_digit()) {
                    continue;
                }
                if let Ok(mut i) = key.parse::<i64>() {
                    if i < 0 {
                        i += a.len() as i64;
                    }
                    if i >= 0 && (i as usize) < a.len() {
                        let mut p = path;
                        p.push(i.to_string());
                        next.push(p);
                    }
                }
            } else if node.is_none() || node.is_some_and(Value::is_object) {
                let mut p = path;
                p.push(key.clone());
                next.push(p);
            }
        }
        paths = next;
    }
    paths
}
pub fn set(root: &mut Value, path: &[String], value: Value) {
    if path.is_empty() {
        *root = value;
        return;
    }
    let mut node = root;
    for key in &path[..path.len() - 1] {
        if node.is_array() {
            let Ok(i) = key.parse::<usize>() else { return };
            let Some(child) = node.as_array_mut().and_then(|a| a.get_mut(i)) else {
                return;
            };
            if !child.is_object() && !child.is_array() {
                *child = json!({});
            }
            node = child;
        } else {
            if !node.is_object() {
                *node = json!({});
            }
            let child = node
                .as_object_mut()
                .unwrap()
                .entry(key)
                .or_insert(json!({}));
            if !child.is_object() && !child.is_array() {
                *child = json!({});
            }
            node = child;
        }
    }
    let key = path.last().unwrap();
    if let Some(a) = node.as_array_mut() {
        if let Ok(i) = key.parse::<usize>() {
            if let Some(v) = a.get_mut(i) {
                *v = value;
            }
        }
    } else if let Some(o) = node.as_object_mut() {
        o.insert(key.clone(), value);
    }
}
pub fn remove(root: &mut Value, path: &[String]) {
    let Some((key, parent)) = path.split_last() else {
        return;
    };
    let mut node = root;
    for key in parent {
        if node.is_array() {
            let Some(v) = key
                .parse::<usize>()
                .ok()
                .and_then(|i| node.as_array_mut().and_then(|a| a.get_mut(i)))
            else {
                return;
            };
            node = v;
        } else {
            let Some(v) = node.get_mut(key) else { return };
            node = v;
        }
    }
    if let Some(a) = node.as_array_mut() {
        if let Ok(i) = key.parse::<usize>() {
            if i < a.len() {
                a.remove(i);
            }
        }
    } else if let Some(o) = node.as_object_mut() {
        o.shift_remove(key);
    }
}
pub fn lookup<'a>(root: &'a Value, path: &Value) -> Option<&'a Value> {
    let paths = resolve(root, &split(path));
    if paths.len() != 1 {
        return None;
    }
    get(root, &paths[0])
}
