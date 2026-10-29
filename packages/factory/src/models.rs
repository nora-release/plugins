use super::network;
use serde_json::{json, Value};
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Wire {
    Anthropic,
    Responses,
    Chat,
    Gemini,
}
impl Wire {
    pub fn api(self) -> &'static str {
        match self {
            Self::Anthropic => "anthropic-messages",
            Self::Responses => "openai-responses",
            Self::Chat => "openai-completions",
            Self::Gemini => "google-generative-ai",
        }
    }
    pub fn path(self) -> &'static str {
        match self {
            Self::Anthropic => "/api/llm/a/v1",
            Self::Responses | Self::Chat => "/api/llm/o/v1",
            Self::Gemini => "/api/llm/g/v1",
        }
    }
}
pub(crate) fn wire(row: &Value) -> Option<Wire> {
    Some(match row["wire"].as_str()? {
        "anthropic" => Wire::Anthropic,
        "responses" => Wire::Responses,
        "chat" => Wire::Chat,
        "gemini" => Wire::Gemini,
        _ => return None,
    })
}
pub(crate) fn registry() -> Vec<Value> {
    serde_json::from_str(include_str!("catalog.json"))
        .expect("The packaged Droid registry is valid JSON")
}
pub(crate) fn find(id: &str) -> Option<Value> {
    registry().into_iter().find(|row| row["id"] == id)
}
pub(crate) fn ids(core: bool) -> Vec<String> {
    registry()
        .into_iter()
        .filter(|row| {
            !matches!(
                row["upstream"].as_str(),
                Some("anthropic" | "openai" | "xai" | "google")
            ) == core
        })
        .filter_map(|row| row["id"].as_str().map(str::to_owned))
        .collect()
}
pub(crate) fn rate(row: &Value, now: u64) -> (f64, Option<f64>) {
    let rate = row["rate"].as_f64().unwrap_or(1.);
    let promo = match row["id"].as_str().unwrap_or_default() {
        "gpt-5.6-sol" => Some((0.2, "2026-08-22T00:00:00Z", "2026-11-22T00:00:00Z")),
        "mistral-large-4" => Some((0.5, "2026-10-06T00:00:00Z", "2026-10-20T00:00:00Z")),
        "gemini-4-argon" => Some((0.5, "2026-10-13T16:00:00Z", "2026-12-01T08:00:00Z")),
        "gemini-3.8-flash" | "gemini-3.7-flash" => {
            Some((0.5, "2026-08-17T21:10:19Z", "2027-01-01T08:00:00Z"))
        }
        _ => None,
    };
    if let Some((discount, from, to)) = promo {
        let ms = |s: &str| {
            chrono::DateTime::parse_from_rfc3339(s)
                .unwrap()
                .timestamp_millis() as u64
        };
        if now >= ms(from) && now < ms(to) {
            return (
                (rate * (1. - discount) * 10000.).round() / 10000.,
                Some(rate),
            );
        }
    }
    (rate, None)
}
pub(crate) fn output(now: u64) -> Value {
    let models=registry().iter().map(|row|{let w=wire(row).expect("packaged wire");let(rate,old)=rate(row,now);let id=row["id"].as_str().unwrap();let fast=find(&format!("{id}-fast")).is_some();let mut info=json!({"catalogSource":"preset","toolCalling":true,"temperature":row["upstream"]!="openai","reasoningEfforts":row["efforts"],"free":false,"pricingKnown":false,"creditMultiplier":rate});if let Some(old)=old{info["previousCreditMultiplier"]=json!(old);}if fast{info["serviceTiers"]=json!(["standard","priority"]);}json!({"id":id,"name":row["name"],"api":w.api(),"provider":"factory","baseUrl":format!("{}{}",network::API,w.path()),"reasoning":row["efforts"].as_array().is_some_and(|v|!v.is_empty()),"input":if row["images"]==true{json!(["text","image"])}else{json!(["text"])},"contextWindow":row["context"],"maxTokens":row["output"],"info":info})}).collect::<Vec<_>>();
    json!({"models":models,"source":"preset","allowUnknownModels":false,"defaultAPI":"anthropic-messages","defaultBaseURL":network::BASE,"message":"Models follow Factory Droid's compiled registry; Factory does not publish an account model-list endpoint."})
}
#[cfg(test)]
#[path = "../test/models.test.rs"]
mod tests;
