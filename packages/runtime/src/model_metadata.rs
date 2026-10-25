use nora_ai::{Model, ModelPricingTier, ModelsCatalog};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;
use std::sync::OnceLock;

// Magpie's makerCatalogs excludes vendors that also host other makers, and
// makerPriceCatalogs excludes the explicitly named membership catalogs.
const MAKERS: &[&str] = &[
    "anthropic",
    "openai",
    "google",
    "deepseek",
    "xai",
    "moonshotai",
    "zhipuai",
    "minimax",
    "stepfun-ai-step-plan",
    "stepfun-ai",
    "stepfun-step-plan",
    "stepfun",
    "xiaomi",
    "alibaba",
    "mistral",
];

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Request {
    models: Vec<Query>,
    #[serde(default)]
    subscription_accounts_enabled: Option<bool>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Query {
    id: String,
    #[serde(default)]
    same_as: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
struct Price {
    input: f64,
    output: f64,
    cache_read: f64,
    cache_write: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    cache_write_1h: Option<f64>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    tiers: Vec<ModelPricingTier>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
struct Metadata {
    price: Price,
    canonical: String,
}

#[derive(Default, Serialize)]
struct Response {
    models: BTreeMap<String, Metadata>,
    defaults: BTreeMap<String, Metadata>,
}

pub(crate) fn cached(directory: &Path, params: &Value) -> PluginResult<Value> {
    let request: Request = serde_json::from_value(params.clone())
        .map_err(|_| PluginError::invalid("The model metadata request is invalid."))?;
    let _ = request.subscription_accounts_enabled;
    validate(&request.models)?;
    let response = crate::model_catalog::saved(directory)
        .map(|catalog| resolve(&catalog, &request.models))
        .unwrap_or_default();
    serde_json::to_value(response)
        .map_err(|_| PluginError::invalid("The model metadata could not be read."))
}

fn validate(queries: &[Query]) -> PluginResult<()> {
    let valid = |id: &str, max: usize| {
        !id.is_empty()
            && id.len() <= max
            && id.trim() == id
            && !id.chars().any(char::is_control)
            && bare(id).chars().any(char::is_alphanumeric)
    };
    let mut ids = BTreeSet::new();
    if queries.len() > 8192
        || queries.iter().any(|query| {
            !valid(&query.id, 512)
                || !ids.insert(&query.id)
                || query.same_as.as_ref().is_some_and(|same| !valid(same, 512))
        })
    {
        return Err(PluginError::invalid(
            "Choose at most 8192 distinct valid model IDs for metadata.",
        ));
    }
    Ok(())
}

fn resolve(catalog: &ModelsCatalog, queries: &[Query]) -> Response {
    let index = Index::new(catalog);
    let mut response = Response::default();
    for query in queries {
        let own = index.maker_price(&query.id);
        if let Some(metadata) = &own {
            response.defaults.insert(query.id.clone(), metadata.clone());
        }
        if let Some(metadata) = own.or_else(|| {
            query
                .same_as
                .as_ref()
                .and_then(|same| index.maker_price(same))
        }) {
            response.models.insert(query.id.clone(), metadata);
        }
    }
    response
}

struct Index<'a> {
    makers: Vec<Vec<&'a Model>>,
    listed: BTreeSet<String>,
    canonical: BTreeSet<String>,
}

impl<'a> Index<'a> {
    fn new(catalog: &'a ModelsCatalog) -> Self {
        Self {
            makers: MAKERS
                .iter()
                .map(|maker| catalog.models_for(maker))
                .collect(),
            listed: catalog
                .all()
                .into_iter()
                .map(|model| bare(&model.id))
                .collect(),
            canonical: catalog
                .all()
                .into_iter()
                .filter_map(|model| model.info.canonical_model_id.as_deref().map(bare))
                .collect(),
        }
    }

    fn maker_price(&self, id: &str) -> Option<Metadata> {
        priced_names(id)
            .into_iter()
            .find_map(|name| self.priced(&name))
    }

    fn priced(&self, id: &str) -> Option<Metadata> {
        if let Some(found) = self
            .makers
            .iter()
            .flat_map(|models| models.iter())
            .find_map(|model| (model.id == id).then(|| metadata(model)).flatten())
        {
            return Some(found);
        }
        let bare = bare(id);
        let bare = unprofiled(&bare);
        for wanted in [bare, cut_at(bare, '('), cut_at(bare, ':')] {
            if let Some(found) = self
                .makers
                .iter()
                .flat_map(|models| models.iter())
                .find_map(|model| {
                    (self::bare(&model.id) == wanted)
                        .then(|| metadata(model))
                        .flatten()
                })
            {
                return Some(found);
            }
        }
        let dashed = bare.replace('.', "-");
        if let Some(found) = self
            .makers
            .iter()
            .flat_map(|models| models.iter())
            .find_map(|model| {
                (self::bare(&model.id).replace('.', "-") == dashed)
                    .then(|| metadata(model))
                    .flatten()
            })
        {
            return Some(found);
        }
        if let Some(found) = self.canonically_priced(bare) {
            return Some(found);
        }
        self.renamed(bare).and_then(|name| self.priced(name))
    }

    fn canonically_priced(&self, wanted: &str) -> Option<Metadata> {
        for dashed in [false, true] {
            for models in &self.makers {
                let mut result: Option<Metadata> = None;
                let mut agrees = true;
                for model in models {
                    let Some(canonical) = model.info.canonical_model_id.as_deref() else {
                        continue;
                    };
                    let Some((maker, _)) = canonical.split_once('/') else {
                        continue;
                    };
                    if !maker.eq_ignore_ascii_case(&model.provider) {
                        continue;
                    }
                    let matches = if dashed {
                        bare(canonical).replace('.', "-") == wanted.replace('.', "-")
                    } else {
                        bare(canonical) == wanted
                    };
                    let Some(found) = matches.then(|| metadata(model)).flatten() else {
                        continue;
                    };
                    if found.price.is_zero() {
                        continue;
                    }
                    if result
                        .as_ref()
                        .is_some_and(|previous| previous.price != found.price)
                    {
                        agrees = false;
                        break;
                    }
                    result = Some(found);
                }
                if agrees && result.is_some() {
                    return result;
                }
            }
        }
        None
    }

    fn listed(&self, name: &str) -> bool {
        let name = unprofiled(name);
        [name, cut_at(name, '('), cut_at(name, ':')]
            .iter()
            .any(|name| self.listed.contains(*name))
    }

    fn known(&self, name: &str) -> bool {
        self.listed(name) || self.canonical.contains(name)
    }

    fn renamed<'b>(&self, name: &'b str) -> Option<&'b str> {
        if self.listed(name) || self.canonical.contains(name) {
            return None;
        }
        let unmarked = unmarked(name);
        if unmarked != name && self.known(unmarked) {
            return Some(unmarked);
        }
        let rest = unmarked.strip_prefix("claude-").or_else(|| {
            let (prefix, rest) = unmarked.split_once('-')?;
            ((2..=24).contains(&prefix.len())
                && prefix.bytes().all(|byte| byte.is_ascii_lowercase()))
            .then_some(rest)
        })?;
        (rest.bytes().any(|byte| byte.is_ascii_digit()) && self.known(rest)).then_some(rest)
    }
}

fn metadata(model: &Model) -> Option<Metadata> {
    let price = Price::from_model(model)?;
    let canonical = model
        .info
        .canonical_model_id
        .as_deref()
        .filter(|id| {
            id.split_once('/')
                .is_some_and(|(maker, _)| maker.eq_ignore_ascii_case(&model.provider))
        })
        .filter(|id| id.len() <= 512 && !id.chars().any(char::is_control))
        .map(str::to_owned)
        .unwrap_or_else(|| format!("{}/{}", model.provider, model.id));
    Some(Metadata { price, canonical })
}

impl Price {
    fn from_model(model: &Model) -> Option<Self> {
        let mut tiers = model.info.pricing_tiers.clone();
        for tier in &mut tiers {
            tier.cache_write_1h = tier
                .cache_write_1h
                .take()
                .filter(|price| price.as_f64().is_some_and(|price| price > 0.0));
        }
        let price = Self {
            input: model.cost.input,
            output: model.cost.output,
            cache_read: model.cost.cache_read,
            cache_write: model.cost.cache_write,
            cache_write_1h: model
                .info
                .cache_write_1h
                .as_ref()
                .and_then(serde_json::Number::as_f64)
                .filter(|price| *price > 0.0),
            tiers,
        };
        let valid = [
            price.input,
            price.output,
            price.cache_read,
            price.cache_write,
        ]
        .into_iter()
        .chain(price.cache_write_1h)
        .all(|value| value.is_finite() && (0.0..=1_000_000.0).contains(&value));
        (valid
            && (model.info.pricing_known == Some(true)
                || model.info.free == Some(true)
                || !price.is_zero()))
        .then_some(price)
    }

    fn is_zero(&self) -> bool {
        self.input == 0.0
            && self.output == 0.0
            && self.cache_read == 0.0
            && self.cache_write == 0.0
            && self.cache_write_1h.is_none()
            && self.tiers.is_empty()
    }
}

fn bare(id: &str) -> String {
    id.rsplit('/').next().unwrap_or(id).to_lowercase()
}

fn unprofiled(id: &str) -> &str {
    [
        "global.", "us.", "us-gov.", "eu.", "apac.", "jp.", "au.", "ca.", "in.",
    ]
    .iter()
    .find_map(|prefix| id.strip_prefix(prefix).filter(|rest| rest.contains('.')))
    .unwrap_or(id)
}

fn cut_at(id: &str, separator: char) -> &str {
    id.find(separator)
        .filter(|index| *index > 0)
        .map(|index| &id[..index])
        .unwrap_or(id)
}

fn unmarked(id: &str) -> &str {
    let Some((name, mark)) = id.strip_suffix(']').and_then(|id| id.rsplit_once('[')) else {
        return id;
    };
    let Some(number) = mark.strip_suffix(['k', 'm']) else {
        return id;
    };
    if !name.is_empty()
        && number
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
        && number
            .parse::<f64>()
            .is_ok_and(|value| value.is_finite() && value > 0.0)
    {
        return name;
    }
    id
}

fn priced_names(id: &str) -> Vec<String> {
    static GROK_WINDOW: OnceLock<Regex> = OnceLock::new();
    static GROK_EFFORT: OnceLock<Regex> = OnceLock::new();
    let mut names = vec![id.to_owned()];
    if id == "codex-auto-review" {
        names.push("gpt-5.6-luna".into());
    }
    let normalized = id.trim().to_lowercase();
    for regex in [
        GROK_WINDOW.get_or_init(|| Regex::new(r"^((?:.*/)?grok-[0-9][^/@]*)@[0-9]+k?$").unwrap()),
        GROK_EFFORT.get_or_init(|| {
            Regex::new(r"^((?:.*/)?grok-[0-9][^/]*?)-(?:minimal|low|medium|high|xhigh|extra-high)$")
                .unwrap()
        }),
    ] {
        if let Some(captures) = regex.captures(&normalized) {
            names.push(captures[1].to_owned());
        }
    }
    names
}

#[cfg(test)]
#[path = "../test/model_metadata.test.rs"]
mod tests;
