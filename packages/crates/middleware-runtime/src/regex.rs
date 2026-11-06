use crate::value::array;
use regress::{Match, Regex};
use serde_json::Value;

pub struct Expression {
    regex: Regex,
    sticky: bool,
    last_index: usize,
}
impl Expression {
    pub fn new(pattern: &str, flags: &str) -> Result<Self, String> {
        let mut seen = std::collections::BTreeSet::new();
        for c in flags.chars() {
            if !"dgimsuvy".contains(c) || !seen.insert(c) {
                return Err("Invalid regular expression flags".into());
            }
        }
        if seen.contains(&'u') && seen.contains(&'v') {
            return Err("Invalid regular expression flags".into());
        }
        Ok(Self {
            regex: Regex::with_flags(pattern, flags).map_err(|e| e.to_string())?,
            sticky: seen.contains(&'y'),
            last_index: 0,
        })
    }
    // RegExp.test and a subsequent non-global String.replace share lastIndex
    // for sticky expressions. Model mapping intentionally keeps that state.
    pub fn tested_replace(&mut self, text: &str, replacement: &str) -> Option<String> {
        let start = if self.sticky { self.last_index } else { 0 };
        let found = (start <= text.len())
            .then(|| self.regex.find_from(text, start).next())
            .flatten()
            .filter(|m| !self.sticky || m.start() == start);
        let Some(found) = found else {
            self.last_index = 0;
            return None;
        };
        if !self.sticky {
            return Some(self.replace(text, replacement, false));
        }
        self.last_index = found.end();
        let next = self
            .regex
            .find_from(text, self.last_index)
            .next()
            .filter(|m| m.start() == self.last_index);
        let Some(next) = next else {
            self.last_index = 0;
            return Some(text.into());
        };
        self.last_index = next.end();
        Some(format!(
            "{}{}{}",
            &text[..next.start()],
            expand(&next, text, replacement),
            &text[next.end()..]
        ))
    }
    pub fn first<'a>(&self, text: &'a str) -> Option<&'a str> {
        self.regex
            .find(text)
            .filter(|m| !self.sticky || m.start() == 0)
            .map(|m| m.as_str(text))
    }
    pub fn replace(&self, text: &str, replacement: &str, all: bool) -> String {
        self.replace_with(text, all, |m| expand(m, text, replacement))
    }
    pub fn replace_with(
        &self,
        text: &str,
        all: bool,
        mut replacement: impl FnMut(&Match) -> String,
    ) -> String {
        let mut output = String::new();
        let mut end = 0;
        let mut found = false;
        for m in self.regex.find_iter(text) {
            if self.sticky && m.start() != end {
                break;
            }
            output.push_str(&text[end..m.start()]);
            output.push_str(&replacement(&m));
            end = m.end();
            found = true;
            if !all {
                break;
            }
        }
        if !found {
            return text.into();
        }
        output.push_str(&text[end..]);
        output
    }
}

// GetSubstitution: JS replacements differ from Rust regex replacements, in
// particular $0, $<name>, $`, $' and the two-digit capture fallback.
fn expand(m: &Match, text: &str, replacement: &str) -> String {
    let mut output = String::new();
    let chars: Vec<char> = replacement.chars().collect();
    let mut i = 0;
    while i < chars.len() {
        let c = chars[i];
        i += 1;
        if c != '$' || i == chars.len() {
            output.push(c);
            continue;
        }
        match chars[i] {
            '$' => {
                output.push('$');
                i += 1;
            }
            '&' => {
                output.push_str(m.as_str(text));
                i += 1;
            }
            '`' => {
                output.push_str(&text[..m.start()]);
                i += 1;
            }
            '\'' => {
                output.push_str(&text[m.end()..]);
                i += 1;
            }
            '<' if m.named_groups().next().is_some() => {
                if let Some(n) = chars[i + 1..].iter().position(|c| *c == '>') {
                    let name: String = chars[i + 1..i + 1 + n].iter().collect();
                    if let Some(r) = m.named_group(&name) {
                        output.push_str(&text[r]);
                    }
                    i += n + 2;
                } else {
                    output.push('$');
                }
            }
            c if c.is_ascii_digit() => {
                let first = c.to_digit(10).unwrap() as usize;
                let mut n = first;
                let mut used = 1;
                if let Some(second) = chars.get(i + 1).and_then(|c| c.to_digit(10)) {
                    let two = n * 10 + second as usize;
                    if two > 0 && two <= m.captures.len() {
                        n = two;
                        used = 2;
                    }
                }
                if n > 0 && n <= m.captures.len() {
                    if let Some(r) = m.group(n) {
                        output.push_str(&text[r]);
                    }
                    i += used;
                } else {
                    output.push('$');
                }
            }
            _ => output.push('$'),
        }
    }
    output
}
pub fn escape(text: &str) -> String {
    text.chars()
        .flat_map(|c| {
            if ".*+?^${}()|[]\\".contains(c) {
                vec!['\\', c]
            } else {
                vec![c]
            }
        })
        .collect()
}

pub fn matcher(options: &Value, ignore_invalid: bool) -> Result<Option<Expression>, String> {
    let mut parts = vec![];
    for w in array(&options["words"]) {
        if let Some(w) = w.as_str().map(crate::value::trim).filter(|w| !w.is_empty()) {
            parts.push(escape(w));
        }
    }
    for p in array(&options["patterns"]) {
        if let Some(p) = p.as_str().filter(|p| !p.is_empty()) {
            if ignore_invalid && Expression::new(p, "").is_err() {
                continue;
            }
            parts.push(format!("(?:{p})"));
        }
    }
    if parts.is_empty() {
        return Ok(None);
    }
    match Expression::new(&parts.join("|"), "gi") {
        Ok(re) => Ok(Some(re)),
        Err(_) if ignore_invalid => Ok(None),
        Err(e) => Err(e),
    }
}
