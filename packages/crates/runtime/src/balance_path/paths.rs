use serde_json::Value;

const MAX_VALUES: usize = 16_384;

struct Predicate<'a> {
    path: Vec<&'a str>,
    not: bool,
    want: &'a str,
}

struct Segment<'a> {
    key: &'a str,
    predicates: Vec<Predicate<'a>>,
}

pub(super) fn resolve<'a>(
    reply: &'a Value,
    path: &str,
) -> Result<(Vec<Option<&'a Value>>, bool), String> {
    let mut values = vec![Some(reply)];
    let mut many = false;
    for segment in segments(path)? {
        let mut next = Vec::new();
        for value in values {
            next.extend(segment.step(value));
            if next.len() > MAX_VALUES {
                return Err("The balance path selects too many values.".into());
            }
        }
        many |= segment.key == "*" || !segment.predicates.is_empty();
        values = next;
        if !many && (values.len() != 1 || values[0].is_none()) {
            return Err("The balance path is missing from the response.".into());
        }
    }
    Ok((values, many))
}

fn child<'a>(value: &'a Value, key: &str) -> Option<&'a Value> {
    let value = match value {
        Value::Object(values) => values.get(key),
        Value::Array(values) => key
            .parse::<usize>()
            .ok()
            .and_then(|index| values.get(index)),
        _ => None,
    };
    value.filter(|value| !value.is_null())
}

impl Segment<'_> {
    fn step<'a>(&self, value: Option<&'a Value>) -> Vec<Option<&'a Value>> {
        let Some(value) = value else {
            return vec![None];
        };
        if self.key == "*" {
            let values = match value {
                Value::Array(values) => values.iter().collect::<Vec<_>>(),
                Value::Object(values) => {
                    let mut values = values.iter().collect::<Vec<_>>();
                    values.sort_by_key(|(key, _)| *key);
                    values.into_iter().map(|(_, value)| value).collect()
                }
                _ => Vec::new(),
            };
            return values
                .into_iter()
                .filter(|value| self.predicates.is_empty() || self.passes(value))
                .map(|value| (!value.is_null()).then_some(value))
                .collect();
        }
        let selected = if self.key.is_empty() {
            Some(value)
        } else {
            child(value, self.key)
        };
        if self.predicates.is_empty() {
            return vec![selected];
        }
        let Some(selected) = selected else {
            return Vec::new();
        };
        let values = match selected {
            Value::Array(values) => values.iter().collect::<Vec<_>>(),
            _ => vec![selected],
        };
        values
            .into_iter()
            .filter(|value| !value.is_null() && self.passes(value))
            .map(Some)
            .collect()
    }

    fn passes(&self, value: &Value) -> bool {
        self.predicates.iter().all(|predicate| {
            let value = predicate
                .path
                .iter()
                .try_fold(value, |value, key| child(value, key));
            let matches = value.is_some_and(|value| match value {
                Value::String(value) => value == predicate.want,
                Value::Number(value) => value
                    .as_f64()
                    .is_some_and(|value| value.to_string() == predicate.want),
                Value::Bool(value) => value.to_string() == predicate.want,
                _ => false,
            });
            matches != predicate.not
        })
    }
}

fn segments(path: &str) -> Result<Vec<Segment<'_>>, String> {
    let mut segments = Vec::new();
    let mut start = 0;
    let mut bracket = false;
    for (offset, ch) in path.char_indices() {
        match ch {
            '[' if bracket => return Err("A balance filter cannot contain another bracket.".into()),
            '[' => bracket = true,
            ']' if !bracket => {
                return Err(
                    "The balance path has a closing bracket without its opening bracket.".into(),
                )
            }
            ']' => bracket = false,
            '.' if !bracket => {
                segments.push(segment(&path[start..offset])?);
                start = offset + 1;
            }
            _ => {}
        }
    }
    if bracket {
        return Err("The balance path has a [ without its ].".into());
    }
    segments.push(segment(&path[start..])?);
    Ok(segments)
}

fn segment(raw: &str) -> Result<Segment<'_>, String> {
    let (key, mut remaining) = raw.split_once('[').unwrap_or((raw, ""));
    if key.is_empty() && remaining.is_empty() {
        return Err("The balance path contains an empty field.".into());
    }
    let mut predicates = Vec::new();
    while !remaining.is_empty() {
        let (body, after) = remaining
            .split_once(']')
            .ok_or("The balance path has a [ without its ].")?;
        let (left, want) = body
            .split_once('=')
            .ok_or("Use a balance filter such as [status=active].")?;
        let (left, not) = left
            .strip_suffix('!')
            .map(|left| (left, true))
            .unwrap_or((left, false));
        let left = left.trim();
        if left.is_empty() || left.split('.').any(str::is_empty) {
            return Err("A balance filter needs a field name.".into());
        }
        predicates.push(Predicate {
            path: left.split('.').collect(),
            not,
            want: want.trim().trim_matches(['\'', '"']),
        });
        remaining = if after.is_empty() {
            ""
        } else {
            after
                .strip_prefix('[')
                .ok_or("Unexpected text after a balance filter.")?
        };
    }
    Ok(Segment { key, predicates })
}
