use super::*;
use std::collections::{BTreeMap, BTreeSet};

const SKIP: &str = "skip_thought_signature_validator";
const INTERRUPTED: &str =
    "[The result of this tool call is unavailable: the turn was interrupted.]";
#[derive(Default)]
pub(crate) struct Names {
    pub original: BTreeMap<String, String>,
}
impl Names {
    fn insert(&mut self, name: &str) -> String {
        if let Some((wire, _)) = self
            .original
            .iter()
            .find(|(_, original)| original.as_str() == name)
        {
            return wire.clone();
        }
        let valid = !name.is_empty()
            && name.len() <= 64
            && name
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
            && !name.as_bytes()[0].is_ascii_digit();
        let wire = if valid {
            name.into()
        } else {
            format!("tool_{}", &fingerprint(name)[..32])
        };
        self.original.insert(wire.clone(), name.into());
        wire
    }
    pub fn restore(&self, name: &str) -> String {
        let name = name
            .strip_prefix("default_api:")
            .or_else(|| name.strip_prefix("default_api."))
            .unwrap_or(name);
        self.original
            .get(name)
            .cloned()
            .unwrap_or_else(|| name.into())
    }
}
fn safe_id(value: &str) -> String {
    value
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect()
}
fn has_response(parts: &[Value]) -> bool {
    parts
        .iter()
        .any(|part| part.get("functionResponse").is_some())
}
fn role(value: &Value) -> &str {
    if value["role"] == "model" {
        "model"
    } else {
        "user"
    }
}
fn append(rows: &mut Vec<Value>, kind: &str, parts: Vec<Value>) {
    if parts.is_empty() {
        return;
    }
    if let Some(last) = rows.last_mut() {
        if last["role"] == kind
            && has_response(last["parts"].as_array().unwrap()) == has_response(&parts)
        {
            last["parts"].as_array_mut().unwrap().extend(parts);
            return;
        }
    }
    rows.push(json!({"role":kind,"parts":parts}));
}
fn response(call: &Value, ag: bool) -> Value {
    let mut result =
        json!({"name":call["name"],"response":{(if ag{"result"}else{"output"}):INTERRUPTED}});
    if let Some(id) = call.get("id") {
        result["id"] = id.clone();
    }
    json!({"functionResponse":result})
}

pub(crate) fn build(
    app: App,
    model: &str,
    body: &Value,
    project: &str,
) -> PluginResult<(Value, Names)> {
    build_with_effort(app, model, body, project, None)
}

pub(crate) fn build_with_effort(
    app: App,
    model: &str,
    body: &Value,
    project: &str,
    effort: Option<&str>,
) -> PluginResult<(Value, Names)> {
    let ag = app == App::Antigravity;
    let claude = ag && model.to_ascii_lowercase().contains("claude");
    let contents = body["contents"]
        .as_array()
        .filter(|rows| rows.len() <= 100_000)
        .ok_or_else(|| invalid("The Google request must contain a conversation."))?;
    let mut req = body.clone();
    if req.get("systemInstruction").is_some_and(Value::is_object) {
        req["systemInstruction"]["role"] = json!("user");
    }
    let mut names = Names::default();
    for tool in req["tools"].as_array_mut().into_iter().flatten() {
        for declaration in tool["functionDeclarations"]
            .as_array_mut()
            .into_iter()
            .flatten()
        {
            let original = text(declaration.get("name"), 512)
                .ok_or_else(|| invalid("A Code Assist tool name is invalid."))?
                .to_owned();
            declaration["name"] = json!(names.insert(&original));
            if let Some(schema) = declaration
                .get("parametersJsonSchema")
                .or_else(|| declaration.get("parameters"))
                .cloned()
            {
                declaration.as_object_mut().unwrap().remove("parameters");
                declaration
                    .as_object_mut()
                    .unwrap()
                    .remove("parametersJsonSchema");
                declaration[if ag {
                    "parameters"
                } else {
                    "parametersJsonSchema"
                }] = if ag { plain_schema(&schema) } else { schema };
            }
        }
    }
    let mut rows = Vec::new();
    let mut ids = BTreeMap::new();
    for row in contents {
        let kind = role(row);
        let mut parts = Vec::new();
        let signed = row["parts"].as_array().is_some_and(|parts| {
            parts.iter().any(|part| {
                part.get("functionCall").is_some()
                    && text(part.get("thoughtSignature"), 64 * 1024).is_some()
            })
        });
        for raw in row["parts"].as_array().into_iter().flatten() {
            if claude && raw["thought"] == true {
                continue;
            }
            let mut part = raw.clone();
            if claude {
                part.as_object_mut().map(|part| {
                    part.remove("thoughtSignature");
                });
            }
            if let Some(call) = part.get_mut("functionCall") {
                if let Some(name) = text(call.get("name"), 512) {
                    call["name"] = json!(names.insert(name));
                }
                if ag {
                    if let Some(id) = text(call.get("id"), 1024).map(str::to_owned) {
                        let wire = ids.entry(id.clone()).or_insert_with(|| {
                            let safe = safe_id(&id);
                            if safe == id {
                                safe
                            } else {
                                format!(
                                    "{}_{}",
                                    safe.chars().take(40).collect::<String>(),
                                    &fingerprint(&id)[..12]
                                )
                            }
                        });
                        call["id"] = json!(wire);
                    }
                }
                if claude || (!signed && part.get("thoughtSignature").is_none()) {
                    part["thoughtSignature"] = json!(SKIP);
                }
            }
            if let Some(fr) = part.get_mut("functionResponse") {
                if let Some(name) = text(fr.get("name"), 512) {
                    fr["name"] = json!(names.insert(name));
                }
                if ag {
                    if let Some(id) = text(fr.get("id"), 1024).map(str::to_owned) {
                        let wire = ids.get(&id).cloned().unwrap_or_else(|| safe_id(&id));
                        fr["id"] = json!(wire);
                    }
                    if let Some(result) = fr.get_mut("response").and_then(Value::as_object_mut) {
                        if let Some(output) = result.remove("output") {
                            result.insert("result".into(), output);
                        }
                    }
                }
            }
            parts.push(part);
        }
        if kind == "user" && has_response(&parts) && !claude {
            let (tools, said): (Vec<_>, Vec<_>) = parts.into_iter().partition(|part| {
                part.get("functionResponse").is_some()
                    || part.get("inlineData").is_some()
                    || part.get("fileData").is_some()
            });
            append(&mut rows, kind, tools);
            append(&mut rows, kind, said);
        } else {
            append(&mut rows, kind, parts);
        }
    }
    // Every call is answered exactly once, immediately after its call turn.
    let mut paired = Vec::new();
    let mut index = 0;
    while index < rows.len() {
        let row = &rows[index];
        let calls = row["parts"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|part| part.get("functionCall"))
            .cloned()
            .collect::<Vec<_>>();
        if row["role"] != "model" || calls.is_empty() {
            let parts = row["parts"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|part| part.get("functionResponse").is_none())
                .cloned()
                .collect();
            append(&mut paired, role(row), parts);
            index += 1;
            continue;
        }
        append(
            &mut paired,
            "model",
            row["parts"].as_array().unwrap().clone(),
        );
        let next = rows.get(index + 1).filter(|row| row["role"] == "user");
        let candidates = next
            .and_then(|row| row["parts"].as_array())
            .cloned()
            .unwrap_or_default();
        let mut used = BTreeSet::new();
        let mut answers = Vec::new();
        for call in &calls {
            let found = candidates.iter().enumerate().find(|(i, part)| {
                if used.contains(i) {
                    return false;
                }
                let Some(fr) = part.get("functionResponse") else {
                    return false;
                };
                match (
                    call.get("id").and_then(Value::as_str),
                    fr.get("id").and_then(Value::as_str),
                ) {
                    (Some(call), Some(answer)) => call == answer,
                    _ => fr["name"] == call["name"],
                }
            });
            if let Some((i, answer)) = found {
                used.insert(i);
                answers.push(answer.clone());
            } else {
                answers.push(response(call, ag));
            }
        }
        let rest = candidates
            .into_iter()
            .filter(|part| part.get("functionResponse").is_none())
            .collect::<Vec<_>>();
        if claude {
            answers.extend(rest);
            append(&mut paired, "user", answers);
        } else {
            let (images, texts): (Vec<_>, Vec<_>) = rest.into_iter().partition(|part| {
                part.get("inlineData").is_some() || part.get("fileData").is_some()
            });
            answers.extend(images);
            append(&mut paired, "user", answers);
            append(&mut paired, "user", texts);
        }
        index += if next.is_some() { 2 } else { 1 };
    }
    if paired.first().is_some_and(|row| row["role"] == "model") {
        paired.insert(
            0,
            json!({"role":"user","parts":[{"text":"[Continuing from previous AI thoughts...]"}]}),
        );
    }
    if paired.is_empty() || paired.last().is_some_and(|row| row["role"] == "model") {
        paired.push(json!({"role":"user","parts":[{"text":"Please continue."}]}));
    }
    req["contents"] = json!(paired);
    if let Some(config) = req
        .get_mut("toolConfig")
        .and_then(|config| config.get_mut("functionCallingConfig"))
    {
        if let Some(allowed) = config
            .get_mut("allowedFunctionNames")
            .and_then(Value::as_array_mut)
        {
            for name in allowed {
                if let Some(original) = name.as_str() {
                    *name = json!(names.insert(original));
                }
            }
        }
        if claude && config["mode"] == "AUTO" {
            config["mode"] = json!("VALIDATED");
        }
    } else if claude
        && req["tools"]
            .as_array()
            .is_some_and(|tools| !tools.is_empty())
    {
        req["toolConfig"] = json!({"functionCallingConfig":{"mode":"VALIDATED"}});
    }
    if let Some(effort) = effort {
        if req.get("generationConfig").is_none() {
            req["generationConfig"] = json!({});
        }
        let generation = model
            .strip_prefix("gemini-")
            .and_then(|id| id.split('.').next())
            .and_then(|id| id.split('-').next())
            .and_then(|major| major.parse::<u32>().ok())
            .unwrap_or(0);
        if generation >= 3 || model.starts_with("gemini-pro-agent") {
            let at = model
                .rsplit_once('-')
                .map(|(_, level)| level)
                .filter(|level| ["minimal", "low", "medium", "high"].contains(level));
            let level = match at.unwrap_or(effort) {
                "minimal" if at == Some("minimal") => "minimal",
                "none" | "minimal" | "low" => "low",
                "medium" if model.contains("flash") || generation > 3 => "medium",
                _ => "high",
            };
            req["generationConfig"]["thinkingConfig"] =
                json!({"includeThoughts":true,"thinkingLevel":level});
        } else if !model.starts_with("gpt-oss") && (!claude || model.contains("thinking")) {
            let budget = match effort {
                "low" => 4096,
                "medium" => 10000,
                "high" => 24000,
                "xhigh" | "max" | "ultra" => 32000,
                _ => 10000,
            };
            req["generationConfig"]["thinkingConfig"] =
                json!({"includeThoughts":true,"thinkingBudget":budget});
        }
    }
    if let Some(gen) = req
        .get_mut("generationConfig")
        .and_then(Value::as_object_mut)
    {
        if ag && !claude {
            gen.remove("maxOutputTokens");
        }
        if model.starts_with("gpt-oss") || (claude && !model.contains("thinking")) {
            gen.remove("thinkingConfig");
        } else if let Some(config) = gen.get_mut("thinkingConfig").and_then(Value::as_object_mut) {
            config.insert("includeThoughts".into(), json!(true));
            if let Some((_, level)) = model
                .rsplit_once('-')
                .filter(|(_, level)| ["minimal", "low", "medium", "high", "xhigh"].contains(level))
            {
                if model.starts_with("gemini-3") || model.starts_with("gemini-4") {
                    config.remove("thinkingBudget");
                    config.insert("thinkingLevel".into(), json!(level));
                }
            }
            if claude {
                let budget = config
                    .get("thinkingBudget")
                    .and_then(Value::as_u64)
                    .unwrap_or(10000);
                let max = gen
                    .get("maxOutputTokens")
                    .and_then(Value::as_u64)
                    .unwrap_or(budget + 32000);
                if budget >= max {
                    if max > 1024 {
                        gen["thinkingConfig"]["thinkingBudget"] = json!(max - 1);
                    } else {
                        gen.remove("thinkingConfig");
                    }
                } else if !gen.contains_key("maxOutputTokens") {
                    gen.insert("maxOutputTokens".into(), json!(max));
                }
            }
        }
    }
    let mut env = json!({"project":project,"model":model,"request":req});
    let id = nora_ai::oauth_pkce::random_hex(12);
    if ag {
        env["userAgent"] = json!("antigravity");
        env["requestType"] = json!("agent");
        env["requestId"] = json!(format!("agent-{id}"));
        let first = env["request"]["contents"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| row["role"] == "user")
            .and_then(|row| row["parts"].as_array())
            .into_iter()
            .flatten()
            .find_map(|part| part["text"].as_str())
            .unwrap_or(&id);
        let digest = fingerprint(first);
        let value = u64::from_str_radix(&digest[..16], 16).unwrap_or(0) & 0x7fff_ffff_ffff_ffff;
        env["request"]["sessionId"] = json!(format!("-{value}"));
    } else {
        env["user_prompt_id"] = json!(id);
    }
    Ok((env, names))
}

pub(crate) fn plain_schema(raw: &Value) -> Value {
    let defs = raw
        .get("$defs")
        .or_else(|| raw.get("definitions"))
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    fn walk(value: &Value, defs: &serde_json::Map<String, Value>, depth: usize) -> Value {
        if depth > 32 {
            return json!({"type":"object"});
        }
        let Some(map) = value.as_object() else {
            return value.clone();
        };
        if let Some(reference) = map.get("$ref").and_then(Value::as_str) {
            return reference
                .rsplit('/')
                .next()
                .and_then(|name| defs.get(name))
                .map(|value| walk(value, defs, depth + 1))
                .unwrap_or_else(|| json!({"type":"object"}));
        }
        if let Some(all) = map.get("allOf").and_then(Value::as_array) {
            let mut merged = map.clone();
            merged.remove("allOf");
            for value in all {
                if let Some(member) = walk(value, defs, depth + 1).as_object() {
                    for (key, value) in member {
                        if key == "properties" {
                            let object = merged.entry(key.clone()).or_insert_with(|| json!({}));
                            if let (Some(existing), Some(additions)) =
                                (object.as_object_mut(), value.as_object())
                            {
                                existing.extend(additions.clone());
                            }
                        } else if key == "required" {
                            let required = merged.entry(key.clone()).or_insert_with(|| json!([]));
                            if let (Some(existing), Some(additions)) =
                                (required.as_array_mut(), value.as_array())
                            {
                                for item in additions {
                                    if !existing.contains(item) {
                                        existing.push(item.clone());
                                    }
                                }
                            }
                        } else {
                            merged.entry(key.clone()).or_insert_with(|| value.clone());
                        }
                    }
                }
            }
            return walk(&json!(merged), defs, depth + 1);
        }
        for key in ["anyOf", "oneOf"] {
            if let Some(alts) = map.get(key).and_then(Value::as_array) {
                let mut picked = alts
                    .iter()
                    .find(|value| value["type"] != "null")
                    .map(|value| walk(value, defs, depth + 1))
                    .unwrap_or_else(|| json!({"type":"string"}));
                if picked.get("type").is_none() {
                    picked["type"] = json!("string");
                }
                if alts.iter().any(|value| value["type"] == "null") {
                    picked["nullable"] = json!(true);
                }
                if let Some(description) = map.get("description") {
                    picked["description"] = description.clone();
                }
                return picked;
            }
        }
        let mut out = serde_json::Map::new();
        for (key, value) in map {
            match key.as_str() {
                "description" | "nullable" | "required" | "pattern" | "minimum" | "maximum"
                | "minLength" | "maxLength" | "minItems" | "maxItems" | "minProperties"
                | "maxProperties" | "propertyOrdering" => {
                    out.insert(key.clone(), value.clone());
                }
                "exclusiveMinimum" | "exclusiveMaximum" if value.is_number() => {
                    let name = if key == "exclusiveMinimum" {
                        "minimum"
                    } else {
                        "maximum"
                    };
                    if !map.contains_key(name) {
                        out.insert(name.into(), value.clone());
                    }
                }
                "type" => {
                    if let Some(types) = value.as_array() {
                        if types.contains(&json!("null")) {
                            out.insert("nullable".into(), json!(true));
                        }
                        if let Some(value) = types.iter().find(|value| **value != "null") {
                            out.insert(key.clone(), value.clone());
                        }
                    } else {
                        out.insert(key.clone(), value.clone());
                    }
                }
                "properties" => {
                    let props = value
                        .as_object()
                        .map(|props| {
                            props
                                .iter()
                                .map(|(name, value)| (name.clone(), walk(value, defs, depth + 1)))
                                .collect::<serde_json::Map<_, _>>()
                        })
                        .unwrap_or_default();
                    out.insert(key.clone(), json!(props));
                }
                "items" => {
                    out.insert(key.clone(), walk(value, defs, depth + 1));
                }
                "enum"
                    if value
                        .as_array()
                        .is_some_and(|values| values.iter().all(Value::is_string)) =>
                {
                    out.insert(key.clone(), value.clone());
                }
                "const" if value.is_string() => {
                    out.insert("enum".into(), json!([value]));
                }
                _ => {}
            }
        }
        if !out.contains_key("type") {
            if out.contains_key("properties") {
                out.insert("type".into(), json!("object"));
            } else if out.contains_key("items") {
                out.insert("type".into(), json!("array"));
            }
        }
        if let Some(required) = out.get("required").and_then(Value::as_array) {
            let keep = required
                .iter()
                .filter(|name| {
                    name.as_str().is_some_and(|name| {
                        out.get("properties")
                            .is_some_and(|props| props.get(name).is_some())
                    })
                })
                .cloned()
                .collect::<Vec<_>>();
            if keep.is_empty() {
                out.remove("required");
            } else {
                out.insert("required".into(), json!(keep));
            }
        }
        json!(out)
    }
    walk(raw, &defs, 0)
}
#[cfg(test)]
#[path = "../test/envelope.test.rs"]
mod tests;
