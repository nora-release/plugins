use super::*;

pub(super) fn append(items: &mut Vec<Value>, summary: Option<&Value>) {
    let mut groups = Vec::new();
    if let Some(summary_groups) = summary.and_then(|value| value["groups"].as_array()) {
        for (index, group) in summary_groups.iter().enumerate() {
            let Some(name) = text(group.get("displayName"), 512) else {
                continue;
            };
            let pool = pool_name(name);
            if pool.is_empty() {
                continue;
            }
            let before = items.len();
            for (bucket_index, bucket) in group["buckets"]
                .as_array()
                .into_iter()
                .flatten()
                .enumerate()
            {
                if bucket["disabled"] == true {
                    continue;
                }
                let span = match bucket["window"]
                    .as_str()
                    .unwrap_or_default()
                    .to_ascii_lowercase()
                    .as_str()
                {
                    "5h" => Some(18000),
                    "weekly" => Some(604800),
                    _ => None,
                };
                let remaining = match bucket.get("remainingFraction") {
                    None => Some(0.0),
                    Some(value) => number(value),
                };
                let Some(remaining) = remaining else {
                    continue;
                };
                let label = match span {
                    Some(18000) => "5 hours",
                    Some(604800) => "7 days",
                    _ => match ["displayName", "window", "bucketId"]
                        .into_iter()
                        .find_map(|key| text(bucket.get(key), 128))
                    {
                        Some(label) => label,
                        None => continue,
                    },
                };
                if let Some(mut value) = item(
                    &format!("pool:{index}:{bucket_index}"),
                    label,
                    remaining,
                    bucket.get("resetTime"),
                    Vec::new(),
                    span,
                    false,
                ) {
                    value["pool"] = json!(pool);
                    items.push(value);
                }
            }
            if items.len() > before {
                // The model-specific match precedes a broader family pool,
                // independently of the order Google returned its groups in.
                let five = items[before..]
                    .iter()
                    .any(|item| item["windowSeconds"] == 18000);
                groups.push(Pool::new(&pool, name, group, five));
            }
        }
    }
    for item in items.iter_mut().filter(|item| item.get("family").is_some()) {
        let named = groups
            .iter()
            .find(|group: &&Pool| group.versioned && pool_names(&group.name, item));
        let family = item["family"]
            .as_str()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if let Some(group) = named.or_else(|| {
            groups
                .iter()
                .find(|group| group.five && !group.versioned && group.about.contains(&family))
        }) {
            item["pool"] = json!(group.name);
        }
    }
}

struct Pool {
    name: String,
    about: String,
    five: bool,
    versioned: bool,
}
impl Pool {
    fn new(pool: &str, name: &str, group: &Value, five: bool) -> Self {
        Self {
            name: pool.into(),
            about: format!(
                "{name} {}",
                text(group.get("description"), 4096).unwrap_or_default()
            )
            .to_ascii_lowercase(),
            five,
            versioned: pool.bytes().any(|b| b.is_ascii_digit()),
        }
    }
}
fn pool_names(pool: &str, item: &Value) -> bool {
    let pool = pool.to_ascii_lowercase();
    [
        item["label"].as_str().unwrap_or_default().into(),
        item["id"].as_str().unwrap_or_default().replace('-', " "),
    ]
    .into_iter()
    .any(|name: String| {
        let name = name.to_ascii_lowercase();
        name == pool
            || name.starts_with(&format!("{pool} "))
            || name.starts_with(&format!("{pool}("))
    })
}
fn pool_name(name: &str) -> String {
    let name = name.trim();
    let name = if name.to_ascii_lowercase().ends_with(" models") {
        name[..name.len() - 7].trim()
    } else {
        name
    };
    name.replace(" and ", " & ")
}
