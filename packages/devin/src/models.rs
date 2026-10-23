use super::{
    network,
    protobuf::{fields, Pb},
};
use nora_ai::{HttpClient, HttpMethod, HttpRequest};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
const LEVELS: &[&str] = &["none", "minimal", "low", "medium", "high", "xhigh", "max"];
#[derive(Clone, Debug)]
pub(crate) struct Model {
    pub id: String,
    pub name: String,
    pub family: String,
    pub family_name: String,
    pub context: u64,
    pub output: u64,
    pub image: bool,
    pub free: bool,
    pub cost: Option<Value>,
}
fn invalid() -> PluginError {
    PluginError::transport("Devin returned an invalid model registry.")
}
pub(crate) fn parse(bytes: &[u8]) -> PluginResult<Vec<Model>> {
    let mut output = Vec::new();
    let mut seen = BTreeSet::new();
    for field in fields(bytes)? {
        if field.key != 1 || field.wire != 2 {
            continue;
        }
        if output.len() >= 4096 {
            return Err(invalid());
        }
        let (mut id, mut name, mut family, mut family_name) =
            (String::new(), String::new(), String::new(), String::new());
        let (mut context, mut max, mut image, mut free) = (0, 0, false, false);
        let mut prices = BTreeMap::new();
        for field in fields(field.data)? {
            match (field.key, field.wire) {
                (22, 2) => id = field.text()?.into(),
                (1, 2) => name = field.text()?.into(),
                (5, 0) => image = field.number != 0,
                (24, 0) => free = field.number == 4,
                (18, 0) => context = field.number,
                (23, 2) => {
                    for f in fields(field.data)? {
                        match (f.key, f.wire) {
                            (13, 0) => max = f.number,
                            (23, 2) => family = f.text()?.into(),
                            _ => {}
                        }
                    }
                }
                (30, 2) => {
                    for f in fields(field.data)? {
                        if f.key == 1 && f.wire == 2 {
                            family_name = f.text()?.into();
                        }
                    }
                }
                (32, 2) => {
                    let (mut label, mut unit, mut amount) = (String::new(), String::new(), None);
                    for f in fields(field.data)? {
                        match (f.key, f.wire) {
                            (1, 2) => label = f.text()?.into(),
                            (3, 2) => unit = f.text()?.into(),
                            (2, 5) => {
                                let n =
                                    f32::from_le_bytes(f.data.try_into().map_err(|_| invalid())?)
                                        as f64;
                                if n.is_finite() && n >= 0. {
                                    let scale = if n > 0. {
                                        10f64.powf(5. - n.log10().floor())
                                    } else {
                                        1.
                                    };
                                    amount = Some((n * scale).round() / scale);
                                }
                            }
                            _ => {}
                        }
                    }
                    if unit == "1M tokens" {
                        if let Some(amount) = amount {
                            prices.insert(label, amount);
                        }
                    }
                }
                _ => {}
            }
        }
        if id.is_empty() {
            continue;
        }
        if id.len() > 512 || id.chars().any(char::is_control) || !seen.insert(id.clone()) {
            return Err(invalid());
        }
        if family.is_empty() {
            family = if !family_name.is_empty() {
                family_name.clone()
            } else if !name.is_empty() {
                name.clone()
            } else {
                id.clone()
            };
        }
        if matches!(family.to_ascii_lowercase().as_str(), "adaptive" | "fusion") {
            continue;
        }
        if family_name.is_empty() {
            family_name = if name.is_empty() {
                family.clone()
            } else {
                name.clone()
            };
        }
        if name.is_empty() {
            name = id.clone();
        }
        let cost = if prices.get("Input").copied().unwrap_or(0.) > 0.
            || prices.get("Output").copied().unwrap_or(0.) > 0.
        {
            Some(
                json!({"input":prices.get("Input").copied().unwrap_or(0.),"output":prices.get("Output").copied().unwrap_or(0.),"cacheRead":prices.get("Cached input").copied().unwrap_or(0.),"cacheWrite":0}),
            )
        } else {
            None
        };
        output.push(Model {
            id,
            name,
            family,
            family_name,
            context,
            output: max,
            image,
            free,
            cost,
        });
    }
    if output.is_empty() {
        return Err(invalid());
    }
    Ok(output)
}
pub(crate) async fn fetch(
    client: &dyn HttpClient,
    key: &str,
    server: &str,
) -> PluginResult<Vec<Model>> {
    let response = network::send(
        client,
        HttpRequest::new(HttpMethod::Post, format!("{server}{}", network::MODELS))
            .with_header("content-type", "application/proto")
            .with_header("connect-protocol-version", "1")
            .with_body(Pb::new().bytes(1, network::metadata(key)).0),
    )
    .await?;
    let status = response.status;
    let bytes = network::collect(response, 16 * 1024 * 1024).await?;
    if !(200..300).contains(&status) {
        return Err(network::error(network::status(
            status,
            &serde_json::from_slice(&bytes).unwrap_or(Value::Null),
        )));
    }
    parse(&bytes)
}
pub(crate) fn level(id: &str) -> (&str, &str) {
    let low = id.to_ascii_lowercase();
    for tier in ["fast", "priority", ""] {
        let base = if tier.is_empty() {
            low.as_str()
        } else if let Some(base) = low
            .strip_suffix(&format!("-{tier}"))
            .or_else(|| low.strip_suffix(&format!("_{tier}")))
        {
            base
        } else {
            continue;
        };
        for word in [
            "none", "minimal", "min", "low", "medium", "xhigh", "high", "max",
        ] {
            if base.ends_with(&format!("-{word}")) || base.ends_with(&format!("_{word}")) {
                return (tier, if word == "min" { "minimal" } else { word });
            }
        }
    }
    ("", "")
}
fn aliases(family: &str) -> Vec<String> {
    let rows: Value =
        serde_json::from_str(include_str!("snapshot.json")).expect("packaged Devin families");
    rows.as_array()
        .unwrap()
        .iter()
        .find(|row| row[0] == family)
        .and_then(|r| r[2].as_array())
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect()
}
pub(crate) fn variant(rows: &[Model], id: &str, effort: &str) -> PluginResult<String> {
    if rows.iter().any(|m| m.id == id) {
        return Ok(id.into());
    }
    let mut family = id;
    let mut tier = "";
    for t in ["fast", "priority"] {
        if let Some(base) = id.strip_suffix(&format!("-{t}")) {
            if rows
                .iter()
                .any(|m| m.family == base || aliases(&m.family).iter().any(|a| a == base))
            {
                family = base;
                tier = t;
                break;
            }
        }
    }
    let members = rows
        .iter()
        .filter(|m| m.family == family || aliases(&m.family).iter().any(|a| a == family))
        .collect::<Vec<_>>();
    let first = members
        .first()
        .ok_or_else(|| PluginError::invalid("This model is not offered by the Devin account."))?;
    if effort.is_empty() && tier.is_empty() {
        return Ok(first.id.clone());
    }
    let effort = if effort == "ultra" { "max" } else { effort };
    let effort = if effort.is_empty() {
        let (t, l) = level(&first.id);
        if t.is_empty() && !l.is_empty() {
            l
        } else {
            "medium"
        }
    } else {
        effort
    };
    if !LEVELS.contains(&effort) {
        return Err(PluginError::invalid(
            "The Devin reasoning effort is unsupported.",
        ));
    }
    let want = LEVELS.iter().position(|l| *l == effort).unwrap();
    let selected = members
        .iter()
        .filter_map(|m| {
            let (t, l) = level(&m.id);
            if t != tier {
                return None;
            }
            let rank = LEVELS.iter().position(|e| *e == l)?;
            Some((m, rank.abs_diff(want), std::cmp::Reverse(rank)))
        })
        .min_by_key(|(_, distance, tie)| (*distance, *tie))
        .map(|(m, _, _)| m.id.clone());
    selected
        .or_else(|| {
            if tier.is_empty() {
                Some(first.id.clone())
            } else {
                None
            }
        })
        .ok_or_else(|| PluginError::invalid("This Devin family offers no requested service tier."))
}
fn entries(rows: &[Model]) -> Vec<(String, String, Vec<&Model>)> {
    let mut out = Vec::new();
    let mut seen = BTreeSet::new();
    for row in rows {
        if !seen.insert(row.family.clone()) {
            continue;
        }
        let members = rows
            .iter()
            .filter(|m| m.family == row.family)
            .collect::<Vec<_>>();
        let mut normal = members
            .iter()
            .copied()
            .filter(|m| {
                let (t, l) = level(&m.id);
                t.is_empty() && !l.is_empty() && m.id != row.family
            })
            .collect::<Vec<_>>();
        if let Some(first) = members.first().copied() {
            if !normal.iter().any(|m| m.id == first.id) {
                normal.push(first);
            }
        }
        out.push((
            row.family.clone(),
            row.family_name.clone(),
            if normal.is_empty() {
                members.clone()
            } else {
                normal
            },
        ));
        for (tier, label) in [("fast", "Fast"), ("priority", "Priority")] {
            let variants = members
                .iter()
                .copied()
                .filter(|m| level(&m.id).0 == tier)
                .collect::<Vec<_>>();
            if !variants.is_empty()
                && !rows
                    .iter()
                    .any(|m| m.family == format!("{}-{tier}", row.family))
            {
                out.push((
                    format!("{}-{tier}", row.family),
                    format!("{} {label}", row.family_name),
                    variants,
                ));
            }
        }
        for m in &members {
            if m.id != row.family && level(&m.id).1.is_empty() {
                out.push((m.id.clone(), m.name.clone(), vec![*m]));
            }
        }
    }
    out
}
pub(crate) fn output(rows: &[Model]) -> Value {
    let mut seen = BTreeSet::new();
    let list=entries(rows).into_iter().filter(|(id,_,_)|seen.insert(id.clone())).map(|(id,name,members)|{let first=members[0];let efforts=LEVELS.iter().copied().filter(|l|members.iter().any(|m|level(&m.id).1==*l)).collect::<Vec<_>>();let free=members.iter().all(|m|m.free);let cost=first.cost.as_ref().filter(|cost|members.iter().all(|m|m.cost.as_ref()==Some(*cost)));let mut info=json!({"toolCalling":true,"reasoningEfforts":efforts,"free":free,"pricingKnown":free||cost.is_some(),"catalogSource":"remote"});if !free&&cost.is_none(){info["pricingKnown"]=json!(false);}json!({"id":id,"name":name,"provider":"devin","api":"openai-completions","baseUrl":network::BASE,"contextWindow":members.iter().find(|m|m.context>0).map(|m|m.context).unwrap_or(0),"maxTokens":members.iter().find(|m|m.output>0).map(|m|m.output).unwrap_or(0),"input":if members.iter().all(|m|m.image){vec!["text","image"]}else{vec!["text"]},"reasoning":!efforts.is_empty(),"cost":if free{json!({"input":0,"output":0,"cacheRead":0,"cacheWrite":0})}else{cost.cloned().unwrap_or(json!({"input":0,"output":0}))},"info":info})}).collect::<Vec<_>>();
    json!({"models":list,"source":"remote","allowUnknownModels":false,"defaultAPI":"openai-completions","defaultBaseURL":network::BASE})
}
pub(crate) fn paid_ids(rows: &[Model]) -> Vec<String> {
    let mut ids = rows
        .iter()
        .filter(|m| !m.free)
        .map(|m| m.id.clone())
        .collect::<BTreeSet<_>>();
    for (id, _, members) in entries(rows) {
        if !members.iter().all(|m| m.free) {
            ids.insert(id);
        }
    }
    ids.into_iter().collect()
}
#[cfg(test)]
#[path = "../test/models.test.rs"]
mod tests;
