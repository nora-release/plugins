use regex::Regex;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default, rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    pub redact: bool,
    pub redact_personal: bool,
    pub redact_shape: bool,
    pub redact_kinds: BTreeMap<String, bool>,
    pub redact_words: Vec<String>,
    pub redact_words_any_case: bool,
    pub redact_rules: Vec<Rule>,
    pub redact_actions: BTreeMap<String, String>,
    pub redact_allow: Vec<String>,
}
#[derive(Clone, Default, Deserialize, Serialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(default, deny_unknown_fields)]
pub struct Rule {
    pub kind: String,
    pub prefix: String,
    pub regex: String,
}
impl Rule {
    pub fn compile(&self) -> Result<Regex, String> {
        let pattern = if self.prefix.is_empty() {
            self.regex.clone()
        } else {
            format!("{}[A-Za-z0-9_\\-]{{8,}}", regex::escape(&self.prefix))
        };
        Regex::new(&pattern).map_err(|_| "A masking rule has an invalid regular expression.".into())
    }
}
pub fn kind_name(value: &str) -> String {
    let mut result: String = value
        .trim()
        .to_uppercase()
        .chars()
        .filter_map(|c| match c {
            'A'..='Z' | '0'..='9' | '_' => Some(c),
            ' ' | '-' | '.' => Some('_'),
            _ => None,
        })
        .collect();
    result = result.trim_matches('_').to_owned();
    if result.is_empty() {
        return "CUSTOM".into();
    }
    if !result.as_bytes()[0].is_ascii_alphabetic() {
        result = format!("CUSTOM_{result}")
    }
    result.truncate(result.len().min(24));
    result.trim_end_matches('_').to_owned()
}
pub const CATEGORIES: &[(&str, &str, bool, &[&str])] = &[
    ("ssn", "Social Security numbers", true, &["SSN"]),
    ("passport", "Passport numbers", true, &["PASSPORT"]),
    ("iban", "IBANs", true, &["IBAN"]),
    ("birthday", "Birthdays", true, &["BIRTHDAY"]),
    ("mac", "MAC addresses", true, &["MAC"]),
    ("serial", "Serial numbers", true, &["SERIAL"]),
    ("home", "User name in home folders", false, &["USERNAME"]),
    (
        "userhost",
        "User and computer names",
        false,
        &["USERNAME", "HOSTNAME"],
    ),
    ("ip", "Public IP addresses", false, &["IP"]),
    ("bucket", "Cloud storage buckets", false, &["BUCKET"]),
];
impl Settings {
    pub fn wanted(&self) -> bool {
        self.redact || self.redact_personal || !self.redact_words.is_empty()
    }
    pub fn covers(&self, category: &str) -> bool {
        match category {
            "" => self.redact,
            "personal" => self.redact_personal,
            _ => {
                self.redact_personal
                    && self
                        .redact_kinds
                        .get(category)
                        .copied()
                        .unwrap_or_else(|| CATEGORIES.iter().any(|c| c.0 == category && c.2))
            }
        }
    }
    pub fn validate(mut self) -> Result<Self, String> {
        self.redact_rules
            .retain(|r| !r.prefix.trim().is_empty() || !r.regex.trim().is_empty());
        let mut rules = BTreeSet::new();
        for rule in &mut self.redact_rules {
            rule.kind = kind_name(&rule.kind);
            rule.prefix = rule.prefix.trim().into();
            rule.regex = rule.regex.trim().into();
            if rule.prefix.is_empty() == rule.regex.is_empty() {
                return Err(
                    "Each masking rule needs either a prefix or a regular expression.".into(),
                );
            }
            if (!rule.prefix.is_empty() && !(3..=64).contains(&rule.prefix.len()))
                || rule.regex.len() > 300
            {
                return Err("A masking rule exceeds its size limit.".into());
            }
            if rule.compile()?.is_match("") {
                return Err("A masking rule cannot match empty text.".into());
            }
        }
        self.redact_rules.retain(|r| rules.insert(r.clone()));
        if self.redact_rules.len() > 32 {
            return Err("At most 32 masking rules are allowed.".into());
        }
        self.redact_actions = self
            .redact_actions
            .into_iter()
            .map(|(k, v)| (k.trim().to_owned(), v.trim().to_lowercase()))
            .filter(|(_, v)| !v.is_empty() && v != "mask")
            .collect();
        if self
            .redact_actions
            .iter()
            .any(|(k, v)| kind_name(k) != *k || !matches!(v.as_str(), "warn" | "block"))
        {
            return Err("Invalid masking action.".into());
        }
        let mut allow = BTreeSet::new();
        self.redact_allow = self
            .redact_allow
            .into_iter()
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty() && allow.insert(v.to_lowercase()))
            .collect();
        if self.redact_allow.len() > 200 || self.redact_allow.iter().any(|v| v.len() > 256) {
            return Err("The allow list exceeds its limit.".into());
        }
        if self.redact_words.len() > 2000 || self.redact_words.iter().any(|v| v.len() > 4096) {
            return Err("The word list exceeds its limit.".into());
        }
        Ok(self)
    }
    pub fn metadata(&self) -> Value {
        let categories: Vec<_> = CATEGORIES
            .iter()
            .map(|c| json!({"id":c.0,"label":c.1,"on":c.2,"kinds":c.3}))
            .collect();
        let mut kinds = BTreeSet::new();
        if self.redact {
            kinds.extend(
                ["API_KEY", "TOKEN", "SECRET", "PASSWORD", "PRIVATE_KEY"].map(str::to_owned),
            );
            kinds.extend(self.redact_rules.iter().map(|r| r.kind.clone()));
        }
        if !self.redact_words.is_empty() {
            kinds.insert("TERM".into());
        }
        if self.redact_personal {
            kinds.extend(["EMAIL", "PHONE", "ID_CARD", "BANK_CARD"].map(str::to_owned));
            for c in CATEGORIES {
                if self.covers(c.0) {
                    kinds.extend(c.3.iter().map(|s| s.to_string()));
                }
            }
        }
        kinds.extend(self.redact_actions.keys().cloned());
        json!({"categories":categories,"actionKinds":kinds})
    }
}
