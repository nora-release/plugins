use super::{auth, network};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
const RANK: [&str; 7] = ["none", "minimal", "low", "medium", "high", "xhigh", "max"];
#[derive(Clone)]
pub(crate) struct Entry {
    pub id: String,
    pub name: String,
    pub context: u64,
    pub size: String,
    pub raw: Value,
    pub variants: Vec<Value>,
    pub default: Value,
    pub partial: bool,
    pub pid: String,
    pub levels: BTreeMap<String, String>,
    pub fast: bool,
}
#[derive(Clone, Debug)]
pub(crate) struct Selection {
    pub id: String,
    pub params: BTreeMap<String, String>,
    pub max_mode: bool,
}
pub(crate) fn parameters(value: &Value) -> BTreeMap<String, String> {
    value["parameterValues"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|p| Some((p["id"].as_str()?.into(), p["value"].as_str()?.into())))
        .collect()
}
fn definitions(value: &Value) -> BTreeMap<String, Vec<String>> {
    value["parameterDefinitions"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|d| {
            let id = d["id"].as_str()?;
            let t = &d["parameterType"];
            let values = t
                .get("enumParameter")
                .or_else(|| t.get("booleanParameter"))?["values"]
                .as_array()?
                .iter()
                .filter_map(|v| v["value"].as_str().map(str::to_owned))
                .collect();
            Some((id.into(), values))
        })
        .collect()
}
pub(crate) fn tokens(value: &str) -> u64 {
    let (number, scale) = if let Some(s) = value.strip_suffix(['k', 'K']) {
        (s, 1000.0)
    } else if let Some(s) = value.strip_suffix(['m', 'M']) {
        (s, 1_000_000.0)
    } else {
        return 0;
    };
    number
        .parse::<f64>()
        .ok()
        .filter(|n| n.is_finite() && *n > 0.0)
        .map(|n| (n * scale).round() as u64)
        .unwrap_or(0)
}
fn default(variants: &[Value]) -> Value {
    variants
        .iter()
        .find(|v| v["isDefaultNonMaxConfig"] == true)
        .or_else(|| variants.iter().find(|v| v["isDefaultMaxConfig"] == true))
        .or_else(|| variants.first())
        .cloned()
        .unwrap_or(Value::Null)
}
fn max(raw: &Value, v: &Value) -> bool {
    raw["supportsNonMaxMode"] == false || v["isMaxMode"] == true
}
pub(crate) fn parse(picker: &Value, usable: Option<&Value>) -> PluginResult<Vec<Entry>> {
    let rows = picker["models"]
        .as_array()
        .filter(|v| v.len() <= 4096)
        .ok_or_else(|| PluginError::transport("Cursor returned an invalid model picker."))?;
    let ids: BTreeSet<&str> = usable
        .into_iter()
        .flat_map(|u| u["models"].as_array().into_iter().flatten())
        .flat_map(|m| [m["modelId"].as_str(), m["displayModelId"].as_str()])
        .flatten()
        .collect();
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for raw in rows {
        let Some(name) = auth::text(raw.get("name"), 256) else {
            continue;
        };
        if raw["isHidden"] == true
            || raw["isChatOnly"] == true
            || raw["onlySupportsCmdK"] == true
            || raw["supportsAgent"] == false
            || [
                "claude-4.5-haiku",
                "claude-4.5-haiku-thinking",
                "gemini-2.5-pro",
                "gemini-2.5-flash",
                "claude-haiku-4-5",
            ]
            .contains(&name)
        {
            continue;
        }
        let original = raw["variants"].as_array().cloned().unwrap_or_default();
        let filtered: Vec<_> = original
            .iter()
            .filter(|v| v["legacySlug"].as_str().is_none_or(|s| ids.contains(s)))
            .cloned()
            .collect();
        let variants = if !ids.is_empty() && filtered.iter().any(|v| v["legacySlug"].is_string()) {
            filtered
        } else {
            original.clone()
        };
        let partial = variants.len() < original.len();
        let defs = definitions(raw);
        let default = default(&variants);
        let has = |pid: &str, value: &str| {
            !partial
                || !variants.iter().any(|v| parameters(v).contains_key(pid))
                || variants
                    .iter()
                    .any(|v| parameters(v).get(pid).is_some_and(|s| s == value))
        };
        let pid = ["effort", "reasoning_effort", "reasoning"]
            .into_iter()
            .find(|p| defs.get(*p).is_some_and(|v| !v.is_empty()))
            .unwrap_or(
                if defs
                    .get("thinking")
                    .is_some_and(|v| v.contains(&"true".into()) && v.contains(&"false".into()))
                {
                    "thinking"
                } else {
                    ""
                },
            );
        let mut levels = BTreeMap::new();
        if pid == "thinking" {
            for (k, v) in [("none", "false"), ("high", "true")] {
                if has(pid, v) {
                    levels.insert(k.into(), v.into());
                }
            }
        } else if let Some(values) = defs.get(pid) {
            for v in values {
                let level = if v == "extra-high" { "xhigh" } else { v };
                if RANK.contains(&level) && has(pid, v) {
                    levels.insert(level.into(), v.clone());
                }
            }
        }
        let title = auth::text(raw.get("clientDisplayName"), 512)
            .unwrap_or(name)
            .replace('\u{200b}', "")
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        let contexts: Vec<_> = defs
            .get("context")
            .into_iter()
            .flatten()
            .filter(|s| has("context", s))
            .cloned()
            .collect();
        for size in if contexts.is_empty() {
            vec![String::new()]
        } else {
            contexts
        } {
            let id = if size.is_empty() {
                if name == "default" {
                    "auto".into()
                } else {
                    name.into()
                }
            } else {
                format!("{name}@{size}")
            };
            if !seen.insert(id.clone()) {
                continue;
            }
            let context = if !size.is_empty() {
                tokens(&size)
            } else {
                raw[if max(raw, &default) {
                    "contextTokenLimitForMaxMode"
                } else {
                    "contextTokenLimit"
                }]
                .as_u64()
                .unwrap_or(0)
            };
            let fast = variants.iter().any(|v| {
                let p = parameters(v);
                p.get("fast").is_some_and(|s| s == "true")
                    && (size.is_empty() || p.get("context").is_none_or(|s| s == &size))
            });
            out.push(Entry {
                id,
                name: if size.is_empty() {
                    title.clone()
                } else {
                    format!("{title} @ {size}")
                },
                context: if context > 0 { context } else { 200_000 },
                size,
                raw: raw.clone(),
                variants: variants.clone(),
                default: default.clone(),
                partial,
                pid: pid.into(),
                levels: levels.clone(),
                fast,
            });
        }
    }
    if out.is_empty() {
        return Err(PluginError::transport(
            "Cursor listed no usable models for this account.",
        ));
    }
    Ok(out)
}
pub(crate) async fn fetch(
    client: &dyn nora_ai::HttpClient,
    token: &str,
    version: &str,
) -> PluginResult<Vec<Entry>> {
    let picker_request = json!({"useModelParameters":true,"doNotUseMarkdown":true});
    let empty = json!({});
    let (picker, usable) = tokio::join!(
        network::unary(
            client,
            "aiserver.v1.AiService/AvailableModels",
            token,
            version,
            &picker_request
        ),
        network::unary(
            client,
            "agent.v1.AgentService/GetUsableModels",
            token,
            version,
            &empty
        )
    );
    parse(&picker?, usable.as_ref().ok())
}
pub(crate) fn output(entries: &[Entry]) -> Value {
    let rows:Vec<_>=entries.iter().map(|e|json!({"id":e.id,"name":e.name,"provider":"cursor","api":"openai-completions","baseUrl":network::BASE,"contextWindow":e.context,"maxTokens":0,"input":["text","image"],"reasoning":!e.levels.is_empty(),"cost":{"input":0,"output":0,"cacheRead":0,"cacheWrite":0},"info":{"toolCalling":true,"temperature":false,"reasoningEfforts":RANK.into_iter().filter(|s|e.levels.contains_key(*s)).collect::<Vec<_>>(),"serviceTiers":if e.fast{vec!["auto","priority"]}else{vec![]},"free":false}})).collect();
    json!({"models":rows,"source":"remote","allowUnknownModels":true,"defaultAPI":"openai-completions","defaultBaseURL":network::BASE})
}
fn fit<'a>(want: &str, levels: impl Iterator<Item = &'a str>) -> String {
    let want = match want {
        "off" => "none",
        "ultra" => "max",
        v => v,
    };
    let levels: Vec<_> = levels.collect();
    if levels.contains(&want) || levels.is_empty() {
        return want.into();
    }
    let Some(at) = RANK.iter().position(|s| *s == want) else {
        return String::new();
    };
    levels
        .into_iter()
        .filter(|s| *s != "none")
        .filter_map(|s| Some((RANK.iter().position(|p| p == &s)?, s)))
        .min_by_key(|(i, _)| (i.abs_diff(at), std::cmp::Reverse(*i)))
        .map(|(_, s)| s.to_owned())
        .unwrap_or_default()
}
fn nearest<'a>(
    variants: impl Iterator<Item = &'a Value>,
    want: &BTreeMap<String, String>,
) -> Option<&'a Value> {
    let mut best = None;
    let mut score = None;
    for v in variants {
        let p = parameters(v);
        let n = want.iter().filter(|(k, v)| p.get(*k) == Some(*v)).count()
            + usize::from(p.get("context") == want.get("context")) * 100;
        if score.is_none_or(|old| n > old) {
            best = Some(v);
            score = Some(n);
        }
    }
    best
}

fn selected(e: &Entry, effort: &str, fast: bool, seed: Option<&Value>) -> Selection {
    let mut want = parameters(seed.unwrap_or(&e.default));
    if !e.size.is_empty() {
        want.insert("context".into(), e.size.clone());
    }
    let effort = if effort == "off" { "none" } else { effort };
    if e.pid != "thinking"
        && effort == "none"
        && !e.levels.contains_key("none")
        && want.contains_key("thinking")
    {
        want.insert("thinking".into(), "false".into());
    } else if !e.pid.is_empty() {
        let level = if effort.is_empty() {
            String::new()
        } else {
            fit(effort, e.levels.keys().map(String::as_str))
        };
        if let Some(v) = e.levels.get(&level) {
            want.insert(e.pid.clone(), v.clone());
        }
        if seed.is_none()
            && e.pid != "thinking"
            && want.contains_key("thinking")
            && !level.is_empty()
        {
            want.insert("thinking".into(), "true".into());
        }
    }
    if definitions(&e.raw).contains_key("fast") {
        want.insert("fast".into(), fast.to_string());
    }
    let same = |v: &&Value| {
        let p = parameters(v);
        p.iter().all(|(k, v)| want.get(k).is_none_or(|s| s == v))
    };
    let mut variant = e.variants.iter().find(same);
    let normalize_effort = |value: &str| {
        if value == "extra-high" {
            "xhigh".to_owned()
        } else {
            value.to_owned()
        }
    };
    let named_effort = e.variants.iter().any(|v| {
        parameters(v)
            .get(&e.pid)
            .zip(want.get(&e.pid))
            .is_some_and(|(a, b)| normalize_effort(a) == normalize_effort(b))
    });
    let default_params = parameters(&e.default);
    let same_default_branch = want.iter().all(|(k, v)| {
        k == &e.pid || k == "context" || default_params.get(k).is_none_or(|s| s == v)
    });
    if variant.is_none()
        && seed.is_some()
        && !e.pid.is_empty()
        && e.pid != "thinking"
        && (named_effort || !same_default_branch)
    {
        let branch: Vec<_> = e
            .variants
            .iter()
            .filter(|v| {
                let p = parameters(v);
                p.contains_key(&e.pid)
                    && want
                        .iter()
                        .all(|(k, v)| k == &e.pid || p.get(k).is_none_or(|s| s == v))
            })
            .collect();
        let level = fit(
            want.get(&e.pid).map(String::as_str).unwrap_or_default(),
            branch
                .iter()
                .filter_map(|v| parameters(v).get(&e.pid).cloned())
                .collect::<Vec<_>>()
                .iter()
                .map(String::as_str),
        );
        variant = branch.into_iter().find(|v| {
            parameters(v)
                .get(&e.pid)
                .is_some_and(|s| s == &level || s == "extra-high" && level == "xhigh")
        });
    }
    if variant.is_none() && e.partial {
        variant = nearest(e.variants.iter(), &want);
    }
    if let Some(v) = variant {
        want.extend(parameters(v));
    }
    Selection {
        id: variant
            .and_then(|v| v["legacySlug"].as_str())
            .or_else(|| e.default["legacySlug"].as_str())
            .unwrap_or(e.raw["name"].as_str().unwrap_or(&e.id))
            .into(),
        max_mode: variant.map(|v| max(&e.raw, v)).unwrap_or(
            max(&e.raw, &e.default)
                || tokens(&e.size) > e.raw["contextTokenLimit"].as_u64().unwrap_or(u64::MAX),
        ),
        params: want,
    }
}
fn split_id(id: &str) -> (String, String, bool) {
    let (id, fast) = id
        .strip_suffix("-fast")
        .map(|s| (s, true))
        .unwrap_or((id, false));
    let (mut id, mut thinking) = id
        .strip_suffix("-thinking")
        .map(|s| (s, true))
        .unwrap_or((id, false));
    let mut effort = String::new();
    for (word, level) in [
        ("extra-high", "xhigh"),
        ("xhigh", "xhigh"),
        ("minimal", "minimal"),
        ("none", "none"),
        ("low", "low"),
        ("medium", "medium"),
        ("high", "high"),
        ("max", "max"),
    ] {
        if let Some(base) = id.strip_suffix(&format!("-{word}")) {
            if !base.is_empty() {
                id = base;
                effort = level.into();
                break;
            }
        }
    }
    if !thinking {
        if let Some(base) = id.strip_suffix("-thinking") {
            id = base;
            thinking = true;
        }
    }
    (
        format!(
            "{id}{}{}",
            if thinking { "-thinking" } else { "" },
            if fast { "-fast" } else { "" }
        ),
        effort,
        fast,
    )
}
pub(crate) fn select(entries: &[Entry], id: &str, effort: &str, fast: Option<bool>) -> Selection {
    if let Some(e) = entries.iter().find(|e| e.id == id) {
        return selected(e, effort, fast.unwrap_or(false), None);
    }
    for e in entries {
        if let Some(seed) = e.variants.iter().find(|v| v["legacySlug"] == id) {
            let size = parameters(seed).get("context").cloned().unwrap_or_default();
            let e = entries
                .iter()
                .find(|x| x.raw["name"] == e.raw["name"] && x.size == size)
                .unwrap_or(e);
            return selected(
                e,
                effort,
                fast.unwrap_or(parameters(seed).get("fast").is_some_and(|s| s == "true")),
                Some(seed),
            );
        }
    }
    let (family, id_effort, id_fast) = split_id(id);
    for e in entries {
        let variants: Vec<_> = e
            .variants
            .iter()
            .filter(|v| {
                v["legacySlug"]
                    .as_str()
                    .is_some_and(|s| split_id(s).0 == family)
            })
            .collect();
        if let Some(seed) = nearest(variants.into_iter(), &parameters(&e.default)) {
            let size = parameters(seed).get("context").cloned().unwrap_or_default();
            let e = entries
                .iter()
                .find(|x| x.raw["name"] == e.raw["name"] && x.size == size)
                .unwrap_or(e);
            return selected(
                e,
                if effort.is_empty() {
                    &id_effort
                } else {
                    effort
                },
                fast.unwrap_or(
                    id_fast || parameters(seed).get("fast").is_some_and(|s| s == "true"),
                ),
                Some(seed),
            );
        }
    }
    for e in entries {
        if e.raw["name"] == id
            || ["legacySlugs", "idAliases"].into_iter().any(|key| {
                e.raw[key]
                    .as_array()
                    .is_some_and(|v| v.iter().any(|s| s == id))
            })
        {
            return selected(e, effort, fast.unwrap_or(false), None);
        }
    }
    Selection {
        id: if id == "auto" {
            "default".into()
        } else {
            id.into()
        },
        params: BTreeMap::new(),
        max_mode: false,
    }
}

#[cfg(test)]
#[path = "../test/models.test.rs"]
mod tests;
