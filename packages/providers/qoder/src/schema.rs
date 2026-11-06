use serde_json::{json, Map, Value};

const SINGLE: &[&str] = &[
    "items",
    "additionalItems",
    "unevaluatedItems",
    "contains",
    "not",
    "if",
    "then",
    "else",
    "propertyNames",
    "additionalProperties",
    "unevaluatedProperties",
];
const LISTS: &[&str] = &["anyOf", "oneOf", "allOf", "prefixItems"];
const MAPS: &[&str] = &[
    "properties",
    "patternProperties",
    "$defs",
    "definitions",
    "dependentSchemas",
];

// Qoder CN's kmodel rejects an untyped oneOf even when every branch is
// explicitly an object. Match Magpie's bounded, schema-only annotation;
// examples and other data, existing types, and ambiguous unions stay intact.
pub(crate) fn typed_unions(schema: &Value) -> Value {
    let mut root = schema.clone();
    walk(&mut root, "", 0);
    root
}

fn object<'a>(mut branch: &'a Value, root: &'a Value) -> bool {
    let mut seen = Vec::new();
    loop {
        let Some(schema) = branch.as_object() else {
            return false;
        };
        if let Some(kind) = schema.get("type") {
            return kind == "object";
        }
        let Some(reference) = schema.get("$ref").and_then(Value::as_str) else {
            return false;
        };
        if !reference.starts_with("#/") || seen.contains(&reference) || seen.len() > 16 {
            return false;
        }
        let Some(target) = root.pointer(&reference[1..]) else {
            return false;
        };
        seen.push(reference);
        branch = target;
    }
}

fn walk(root: &mut Value, path: &str, depth: usize) {
    if depth > 64 {
        return;
    }
    let Some(schema) = root.pointer(path).and_then(Value::as_object) else {
        return;
    };
    let annotate = !schema.contains_key("type")
        && schema
            .get("oneOf")
            .and_then(Value::as_array)
            .is_some_and(|branches| {
                !branches.is_empty() && branches.iter().all(|branch| object(branch, root))
            });
    let children = children(schema, path);
    if annotate {
        root.pointer_mut(path).unwrap()["type"] = json!("object");
    }
    // Resolve against the evolving copy, including nodes already annotated
    // during this traversal, without modifying the caller's schema.
    for child in children {
        walk(root, &child, depth + 1);
    }
}

fn children(schema: &Map<String, Value>, path: &str) -> Vec<String> {
    let mut paths = Vec::new();
    for keyword in SINGLE {
        if schema.contains_key(*keyword) {
            paths.push(format!("{path}/{keyword}"));
        }
    }
    for keyword in LISTS {
        if let Some(values) = schema.get(*keyword).and_then(Value::as_array) {
            paths.extend((0..values.len()).map(|index| format!("{path}/{keyword}/{index}")));
        }
    }
    for keyword in MAPS {
        if let Some(values) = schema.get(*keyword).and_then(Value::as_object) {
            paths.extend(values.keys().map(|key| {
                let key = key.replace('~', "~0").replace('/', "~1");
                format!("{path}/{keyword}/{key}")
            }));
        }
    }
    paths
}
