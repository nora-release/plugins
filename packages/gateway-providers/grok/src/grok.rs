//! Grok Build's backend (cli-chat-proxy.grok.com), OpenAI's Responses API
//! as Grok Build speaks it: its headers, what a request may carry there,
//! the models an account lists with their context windows, and the credits
//! it has used.

use nora_plugin_runtime_sdk::provider::{Model, Usage, UsageWindow};
use provider_support::HttpRequest;
use serde_json::{json, Map, Value};

pub(crate) const BASE: &str = "https://cli-chat-proxy.grok.com/v1";
/// Grok Build's version when its stable channel can't be read.
pub(crate) const VERSION: &str = "1.0.77";
/// Where Grok Build's installer reads the stable version.
pub(crate) const STABLE_URLS: [&str; 2] = [
    "https://x.ai/cli/stable",
    "https://storage.googleapis.com/grok-build-public-artifacts/cli/stable",
];

/// The tool types Grok's backend takes; it turns the whole request away
/// over another, such as a freeform `apply_patch` or a namespace.
const TOOLS: [&str; 11] = [
    "function",
    "web_search",
    "x_search",
    "image_generation",
    "collections_search",
    "file_search",
    "code_execution",
    "code_interpreter",
    "mcp",
    "shell",
    "tool_search",
];

/// Codex's output token replay, which Grok turns away in `include`.
const OUTPUT_REPLAY: &str = "output.encrypted_content";

/// Grok Build's loop checks: the proxy stops a reply repeating itself after
/// about 1K tokens rather than running on (its 1.0.67 values).
pub(crate) const LOOP_CHECKS: [(&str, &str); 2] = [
    ("x-grok-doom-loop-check", "1024"),
    ("x-grok-exact-repetition-check", "64"),
];

/// A request signed as Grok Build signs it.
pub(crate) fn sign(request: HttpRequest, token: &str, version: &str) -> HttpRequest {
    let arch = if cfg!(target_arch = "aarch64") {
        "aarch64"
    } else {
        "x86_64"
    };
    request
        .with_header("authorization", format!("Bearer {token}"))
        .with_header("x-grok-client-version", version)
        .with_header("x-xai-token-auth", "xai-grok-cli")
        .with_header("x-grok-client-identifier", "grok-shell")
        .with_header("x-grok-client-mode", "headless")
        .with_header(
            "user-agent",
            format!("grok-shell/{version} (macos; {arch})"),
        )
}

/// A version as Grok Build's stable channel names it, or none.
pub(crate) fn version(text: &str) -> Option<String> {
    let line = text.lines().next()?.trim();
    let (core, _) = line.split_once('-').unwrap_or((line, ""));
    let parts: Vec<&str> = core.split('.').collect();
    let numeric = parts.len() == 3
        && parts
            .iter()
            .all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()));
    numeric.then(|| line.to_owned())
}

/// The model a requested id names and the context window it asks for:
/// `grok-4.7@500k` is grok-4.7 at 500000 tokens, `grok-4.7` its default.
pub(crate) fn model_of(id: &str) -> (&str, u64) {
    let Some((model, size)) = id.rsplit_once('@') else {
        return (id, 0);
    };
    let (digits, thousands) = match size.strip_suffix('k') {
        Some(digits) => (digits, 1000),
        None => (size, 1),
    };
    match digits.parse::<u64>() {
        Ok(count) if count > 0 && !model.is_empty() => (model, count * thousands),
        _ => (id, 0),
    }
}

fn label(window: u64) -> String {
    if window.is_multiple_of(1000) {
        format!("{}k", window / 1000)
    } else {
        window.to_string()
    }
}

/// The models the account lists: Responses models only, each at its
/// default window under its own id, and each other window it lists
/// beside it (`grok-4.7@500k`), as Grok Build's /context-window offers.
pub(crate) fn models(value: &Value) -> Vec<Model> {
    let Some(list) = value["data"].as_array() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in list {
        let Some(id) = entry["id"].as_str().filter(|id| !id.is_empty()) else {
            continue;
        };
        if entry["api_backend"]
            .as_str()
            .is_some_and(|backend| backend != "responses")
        {
            continue;
        }
        let name = entry["name"]
            .as_str()
            .filter(|name| !name.is_empty())
            .unwrap_or(id);
        let reasoning = entry["reasoning_efforts"]
            .as_array()
            .is_some_and(|efforts| !efforts.is_empty());
        let images = ["input_modalities", "modalities"].iter().any(|key| {
            entry[*key]
                .as_array()
                .is_some_and(|inputs| inputs.iter().any(|input| input == "image"))
        });
        let default = entry["context_window"].as_u64().unwrap_or(0);
        let mut windows: Vec<u64> = Vec::new();
        for window in entry["context_windows"].as_array().into_iter().flatten() {
            if let Some(window) = window.as_u64().filter(|window| *window > 0) {
                if !windows.contains(&window) {
                    windows.push(window);
                }
            }
        }
        if windows.is_empty() && default > 0 {
            windows.push(default);
        }
        let base = if windows.contains(&default) {
            default
        } else {
            windows.first().copied().unwrap_or(0)
        };
        let model = |id: String, name: String, window: u64| Model {
            name: Some(name),
            context: (window > 0).then_some(window),
            reasoning,
            images,
            ..Model::new(id)
        };
        out.push(model(id.to_owned(), name.to_owned(), base));
        for window in windows.into_iter().filter(|window| *window != base) {
            out.push(model(
                format!("{id}@{}", label(window)),
                format!("{name} @ {}", label(window)),
                window,
            ));
        }
    }
    out
}

/// Leaves out what Grok's backend turns a request away over: tools of
/// other types, a web search's `external_web_access`, a `tool_choice`
/// naming a type left out or with no tools beside it, a reasoning item's
/// `"content": null`, and Codex's output replay in `include`. A function's
/// parameters with a union at the root are folded to an object; a Codex
/// `agent_message` goes as the user's message. Says whether it changed.
pub(crate) fn rewrite(body: &mut Map<String, Value>) -> bool {
    let mut dirty = false;
    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
        let before = tools.len();
        tools.retain(|tool| {
            tool["type"]
                .as_str()
                .is_none_or(|kind| TOOLS.contains(&kind))
        });
        dirty |= tools.len() != before;
        for tool in tools.iter_mut().filter_map(Value::as_object_mut) {
            dirty |= tool.remove("external_web_access").is_some();
            if tool.get("type").and_then(Value::as_str) != Some("function") {
                continue;
            }
            let function = match tool.get_mut("function").and_then(Value::as_object_mut) {
                Some(function) => function,
                None => tool,
            };
            if let Some(parameters) = function.get("parameters") {
                let folded = object_root(parameters);
                if &folded != parameters {
                    function.insert("parameters".into(), folded);
                    dirty = true;
                }
            }
        }
        let choice_dropped = body
            .get("tool_choice")
            .and_then(|choice| choice["type"].as_str())
            .is_some_and(|kind| !TOOLS.contains(&kind));
        if choice_dropped {
            body.remove("tool_choice");
            dirty = true;
        }
    }
    let has_tools = body
        .get("tools")
        .and_then(Value::as_array)
        .is_some_and(|tools| !tools.is_empty());
    if !has_tools && body.remove("tool_choice").is_some() {
        dirty = true;
    }
    if let Some(input) = body.get_mut("input").and_then(Value::as_array_mut) {
        for item in input.iter_mut() {
            if let Some(message) = agent_message(item) {
                *item = message;
                dirty = true;
            }
            if let Some(object) = item.as_object_mut() {
                if object.get("type").and_then(Value::as_str) == Some("reasoning")
                    && object.get("content") == Some(&Value::Null)
                {
                    object.remove("content");
                    dirty = true;
                }
            }
        }
    }
    if let Some(include) = body.get_mut("include").and_then(Value::as_array_mut) {
        let before = include.len();
        include.retain(|value| value != OUTPUT_REPLAY);
        if include.len() != before {
            dirty = true;
            if include.is_empty() {
                body.remove("include");
            }
        }
    }
    dirty
}

/// A plain Codex `agent_message` as the user's message, its sender and
/// recipient said first. A sealed one, or one with other parts, stays.
fn agent_message(item: &Value) -> Option<Value> {
    if item["type"] != "agent_message" || item.get("encrypted_content").is_some() {
        return None;
    }
    let parts = item["content"]
        .as_array()
        .filter(|parts| !parts.is_empty())?;
    let plain = parts.iter().all(|part| {
        part.get("encrypted_content").is_none()
            && (part["type"] == "input_text" && part["text"].is_string()
                || part["type"] == "input_image" && part["image_url"].is_string())
    });
    if !plain {
        return None;
    }
    let from = item["author"].as_str().unwrap_or_default();
    let to = item["recipient"].as_str().unwrap_or_default();
    let header = match (from.is_empty(), to.is_empty()) {
        (false, false) => format!("From {from} to {to}"),
        (false, true) => format!("From {from}"),
        (true, false) => format!("To {to}"),
        (true, true) => String::new(),
    };
    let mut content = Vec::new();
    if !header.is_empty() {
        content.push(json!({"type": "input_text", "text": format!("{header}\n\n")}));
    }
    content.extend(parts.iter().cloned());
    Some(json!({"type": "message", "role": "user", "content": content}))
}

/// A tool's parameters as a plain object at the root, which Grok requires
/// ("tool parameter root must be an object type"): allOf's branches merged;
/// of anyOf's and oneOf's object branches, the properties merged and the
/// fields each requires kept required. Local `$ref`s are followed.
pub(crate) fn object_root(parameters: &Value) -> Value {
    fold(parameters, parameters, 0)
}

fn fold(root: &Value, schema: &Value, depth: usize) -> Value {
    let Some(object) = schema.as_object() else {
        return schema.clone();
    };
    let union = ["anyOf", "oneOf", "allOf"]
        .iter()
        .any(|key| object.contains_key(*key));
    if object.get("type").and_then(Value::as_str) == Some("object") && !union {
        return schema.clone();
    }
    let resolve = |branch: &Value| -> Option<Value> {
        let reference = branch["$ref"]
            .as_str()
            .filter(|reference| !reference.is_empty());
        let Some(reference) = reference else {
            return branch.is_object().then(|| branch.clone());
        };
        ["$defs", "definitions"].iter().find_map(|key| {
            let name = reference.strip_prefix(&format!("#/{key}/"))?;
            root[*key][name]
                .is_object()
                .then(|| root[*key][name].clone())
        })
    };
    let mut out = object.clone();
    let mut properties = object
        .get("properties")
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    let mut required: Vec<Value> = object
        .get("required")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    for branch in object
        .get("allOf")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(resolve)
    {
        for (key, value) in branch["properties"].as_object().into_iter().flatten() {
            properties
                .entry(key.clone())
                .or_insert_with(|| value.clone());
        }
        required.extend(branch["required"].as_array().into_iter().flatten().cloned());
    }
    out.remove("allOf");
    let mut branches = Vec::new();
    for key in ["anyOf", "oneOf"] {
        for branch in object
            .get(key)
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(resolve)
        {
            let nested = ["anyOf", "oneOf", "allOf"]
                .iter()
                .any(|union| branch.get(*union).is_some());
            let branch = if depth < 8 && nested {
                fold(root, &branch, depth + 1)
            } else {
                branch
            };
            if branch["type"] == "object" || branch.get("properties").is_some() {
                branches.push(branch);
            }
        }
        out.remove(key);
    }
    let mut common: Option<Vec<Value>> = None;
    let mut offered: Vec<(String, Vec<Value>)> = Vec::new();
    for branch in &branches {
        for (key, value) in branch["properties"].as_object().into_iter().flatten() {
            if properties.contains_key(key) {
                continue;
            }
            match offered.iter_mut().find(|(name, _)| name == key) {
                Some((_, values)) if !values.contains(value) => values.push(value.clone()),
                Some(_) => {}
                None => offered.push((key.clone(), vec![value.clone()])),
            }
        }
        let needs: Vec<Value> = branch["required"].as_array().cloned().unwrap_or_default();
        common = Some(match common {
            None => needs,
            Some(kept) => kept
                .into_iter()
                .filter(|field| needs.contains(field))
                .collect(),
        });
    }
    for (key, mut values) in offered {
        let value = if values.len() == 1 {
            values.remove(0)
        } else {
            json!({"anyOf": values})
        };
        properties.insert(key, value);
    }
    required.extend(common.unwrap_or_default());
    let mut seen = Vec::new();
    required.retain(|field| {
        let fresh = !seen.contains(field);
        seen.push(field.clone());
        fresh
    });
    out.insert("type".into(), json!("object"));
    out.insert("properties".into(), Value::Object(properties));
    if required.is_empty() {
        out.remove("required");
    } else {
        out.insert("required".into(), Value::Array(required));
    }
    Value::Object(out)
}

/// `/billing?format=credits` as the router reads usage: the credits of the
/// current period, and on-demand spending when it has a cap.
pub(crate) fn usage(value: &Value) -> Usage {
    let config = &value["config"];
    let number = |value: &Value| value.as_f64().unwrap_or(0.0);
    let (name, end) = match config
        .get("currentPeriod")
        .filter(|period| period.is_object())
    {
        Some(period) => {
            let kind = period["type"].as_str().unwrap_or_default();
            let name = match kind.trim_start_matches("USAGE_PERIOD_TYPE_") {
                "DAILY" => "1 day",
                "WEEKLY" => "7 days",
                "MONTHLY" => "Month",
                _ => "Allowance",
            };
            (name, period["end"].as_str())
        }
        None => ("Allowance", config["billingPeriodEnd"].as_str()),
    };
    let resets_at = end.filter(|end| end.len() >= 20).map(str::to_owned);
    let mut windows = vec![UsageWindow {
        name: name.into(),
        used: Some(number(&config["creditUsagePercent"])),
        resets_at: resets_at.clone(),
        ..UsageWindow::default()
    }];
    let cap = number(&config["onDemandCap"]["val"]);
    if cap > 0.0 {
        windows.push(UsageWindow {
            name: "On-demand".into(),
            used: Some(100.0 * number(&config["onDemandUsed"]["val"]) / cap),
            resets_at,
            ..UsageWindow::default()
        });
    }
    Usage {
        plan: config["subscriptionTier"]
            .as_str()
            .or_else(|| value["subscriptionTier"].as_str())
            .filter(|plan| !plan.is_empty())
            .map(str::to_owned),
        windows,
    }
}

#[cfg(test)]
#[path = "../test/grok.test.rs"]
mod tests;
