use crate::{auth, network, pool::Open};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

const RETAIN_MS: u64 = 600_000;
const MAX_DEBTS: usize = 64;

#[derive(Clone, Default, Debug, PartialEq, Eq)]
pub(crate) struct Counts {
    pub input: u64,
    pub output: u64,
    pub cached: u64,
    pub written: u64,
}
impl Counts {
    fn add(&mut self, other: &Self) {
        self.input = self.input.saturating_add(other.input);
        self.output = self.output.saturating_add(other.output);
        self.cached = self.cached.saturating_add(other.cached);
        self.written = self.written.saturating_add(other.written);
    }
    pub fn attach(&self, usage: &mut Value) {
        let prompt = self
            .input
            .saturating_add(self.cached)
            .saturating_add(self.written);
        if prompt == 0 && self.output == 0 {
            return;
        }
        let previous = &usage["magpie_deferred"];
        usage["magpie_deferred"] = json!({
            "prompt_tokens":prompt.saturating_add(previous["prompt_tokens"].as_u64().unwrap_or(0)),
            "completion_tokens":self.output.saturating_add(previous["completion_tokens"].as_u64().unwrap_or(0)),
            "cached_tokens":self.cached.saturating_add(previous["cached_tokens"].as_u64().unwrap_or(0)),
            "cache_write_tokens":self.written.saturating_add(previous["cache_write_tokens"].as_u64().unwrap_or(0)),
        });
    }
    fn parse(value: &Value) -> Option<Self> {
        if !value.is_object()
            || ![
                "inputTokens",
                "outputTokens",
                "cacheReadTokens",
                "cacheWriteTokens",
            ]
            .iter()
            .any(|field| value.get(field).is_some())
        {
            return None;
        }
        let count = |name| match value.get(name) {
            Some(value) => number(value),
            None => Some(0),
        };
        Some(Self {
            input: count("inputTokens")?,
            output: count("outputTokens")?,
            cached: count("cacheReadTokens")?,
            written: count("cacheWriteTokens")?,
        })
    }
}

fn number(value: &Value) -> Option<u64> {
    value.as_u64().or_else(|| value.as_str()?.parse().ok())
}

struct Debt {
    since: u64,
    before: Option<u64>,
}
#[derive(Default)]
struct Carried {
    counts: Counts,
    since: Option<u64>,
}
impl Carried {
    fn add(&mut self, counts: &Counts, since: u64) {
        if *counts == Counts::default() {
            return;
        }
        self.counts.add(counts);
        self.since = Some(self.since.map_or(since, |old| old.min(since)));
    }
    fn merge(&mut self, other: &Self) {
        if let Some(since) = other.since {
            self.add(&other.counts, since);
        }
    }
}
#[derive(Default)]
struct Scope {
    debts: VecDeque<Debt>,
    starts: VecDeque<(u64, u64)>,
    next_admission: u64,
    claimed: BTreeMap<String, u64>,
    carry: Carried,
    touched: u64,
}
#[derive(Default)]
struct State {
    scopes: BTreeMap<String, Scope>,
    unreadable: BTreeMap<String, u64>,
}
impl State {
    fn prune(&mut self, now: u64) {
        self.unreadable.retain(|_, until| *until > now);
        self.scopes.retain(|_, scope| {
            scope
                .debts
                .retain(|debt| now.saturating_sub(debt.since) <= RETAIN_MS);
            scope
                .claimed
                .retain(|_, at| now.saturating_sub(*at) <= RETAIN_MS * 2);
            scope
                .starts
                .retain(|(_, at)| now.saturating_sub(*at) <= RETAIN_MS);
            if scope
                .carry
                .since
                .is_some_and(|since| now.saturating_sub(since) > RETAIN_MS)
            {
                scope.carry = Carried::default();
            }
            now.saturating_sub(scope.touched) <= RETAIN_MS * 2
        });
    }
}
#[derive(Default)]
pub(crate) struct Ledger {
    state: Mutex<State>,
}
pub(crate) struct Book {
    ledger: Arc<Ledger>,
    client: network::Client,
    token: String,
    team_id: Option<u64>,
    version: String,
    account: String,
    conversation: String,
    key: String,
    since: u64,
    admission: u64,
    kept: bool,
    pending: bool,
    final_seen: bool,
    settled: bool,
    collected: Carried,
}
impl Ledger {
    pub fn begin(self: &Arc<Self>, config: &Open, conversation: &str, since: u64) -> Book {
        let key = format!("{}\0{}", config.account, conversation);
        let kept = !config.selector.is_empty();
        let admission = if kept {
            let mut state = self.state.lock().unwrap_or_else(|error| error.into_inner());
            state.prune(auth::now());
            let scope = state.scopes.entry(key.clone()).or_default();
            scope.touched = auth::now();
            for debt in &mut scope.debts {
                debt.before = Some(debt.before.map_or(since, |before| before.min(since)));
            }
            let admission = scope.next_admission;
            scope.next_admission = scope.next_admission.saturating_add(1);
            scope.starts.push_back((admission, since));
            while scope.starts.len() > MAX_DEBTS {
                scope.starts.pop_front();
            }
            admission
        } else {
            0
        };
        Book {
            ledger: self.clone(),
            client: config.client.clone(),
            token: config.token.clone(),
            team_id: config.team_id,
            version: config.version.clone(),
            account: config.account.clone(),
            conversation: conversation.into(),
            key,
            since,
            admission,
            kept,
            pending: false,
            final_seen: false,
            settled: false,
            collected: Carried::default(),
        }
    }
}
impl Book {
    pub fn pending(&mut self) {
        self.pending = true;
    }
    pub fn final_seen(&mut self) {
        self.final_seen = true;
    }
    pub fn delivered(&mut self) {
        self.settled = true;
        self.collected = Carried::default();
    }
    pub fn abandon(&mut self) {
        if self.settled || !self.kept {
            return;
        }
        self.settled = true;
        let mut state = self
            .ledger
            .state
            .lock()
            .unwrap_or_else(|error| error.into_inner());
        let can_read = !state
            .unreadable
            .get(&self.account)
            .is_some_and(|until| *until > auth::now());
        let scope = state.scopes.entry(self.key.clone()).or_default();
        scope.touched = auth::now();
        scope.carry.merge(&self.collected);
        if self.pending
            && !self.final_seen
            && can_read
            && scope
                .starts
                .iter()
                .any(|(admission, _)| *admission == self.admission)
        {
            let before = scope
                .starts
                .iter()
                .copied()
                .filter(|(admission, _)| *admission > self.admission)
                .map(|(_, at)| at)
                .min();
            scope.debts.push_back(Debt {
                since: self.since,
                before,
            });
            while scope.debts.len() > MAX_DEBTS {
                scope.debts.pop_front();
            }
        }
    }
    pub async fn collect(&mut self) -> Counts {
        if !self.kept {
            return Counts::default();
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(6);
        loop {
            let since = {
                let mut state = self
                    .ledger
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                state.prune(auth::now());
                let unreadable = state.unreadable.contains_key(&self.account);
                let Some(scope) = state.scopes.get_mut(&self.key) else {
                    break;
                };
                self.collected.merge(&std::mem::take(&mut scope.carry));
                if unreadable {
                    break;
                }
                let Some(since) = scope.debts.iter().map(|debt| debt.since).min() else {
                    break;
                };
                since
            };
            let mut body = json!({"startDate":since.saturating_sub(60_000).to_string(),"endDate":auth::now().saturating_add(60_000).to_string(),"page":1,"pageSize":50});
            if let Some(team) = self.team_id {
                body["teamId"] = json!(team);
            }
            let request = network::post(
                format!(
                    "{}/aiserver.v1.DashboardService/GetFilteredUsageEvents",
                    network::API
                ),
                body.to_string().into_bytes(),
                &self.token,
                &self.version,
                "application/json",
            );
            let response = tokio::time::timeout(Duration::from_secs(5), async {
                let response = network::send(self.client.as_ref(), request, 5).await?;
                let status = response.status;
                if (200..300).contains(&status) {
                    network::read(response).await.map(|body| (status, body))
                } else {
                    Ok((status, Value::Null))
                }
            })
            .await;
            if let Ok(Ok((status, body))) = response {
                let mut state = self
                    .ledger
                    .state
                    .lock()
                    .unwrap_or_else(|error| error.into_inner());
                if (400..500).contains(&status) && status != 429 {
                    state
                        .unreadable
                        .insert(self.account.clone(), auth::now() + RETAIN_MS);
                    if let Some(scope) = state.scopes.get_mut(&self.key) {
                        scope.debts.clear();
                    }
                    break;
                }
                if let Some(scope) = state.scopes.get_mut(&self.key) {
                    self.collected.add(
                        &claim(scope, &self.conversation, &body, auth::now()),
                        auth::now(),
                    );
                    if scope.debts.is_empty() {
                        break;
                    }
                }
            }
            if tokio::time::Instant::now() + Duration::from_millis(500) >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(500)).await;
        }
        self.collected.counts.clone()
    }
}
impl Drop for Book {
    fn drop(&mut self) {
        self.abandon();
    }
}

fn claim(scope: &mut Scope, conversation: &str, body: &Value, now: u64) -> Counts {
    let mut counts = Counts::default();
    let Some(events) = body["usageEventsDisplay"].as_array() else {
        return counts;
    };
    let mut events: Vec<_> = events
        .iter()
        .filter_map(|event| {
            if event["conversationId"] != conversation {
                return None;
            }
            Some((
                number(&event["timestamp"])?,
                Counts::parse(&event["tokenUsage"])?,
            ))
        })
        .collect();
    events.sort_by_key(|event| event.0);
    let mut occurrences = BTreeMap::<String, usize>::new();
    for (at, usage) in events {
        let fingerprint = auth::hash(format!(
            "{at}\0{}\0{}\0{}\0{}",
            usage.input, usage.output, usage.cached, usage.written
        ));
        let occurrence = occurrences.entry(fingerprint.clone()).or_default();
        let key = format!("{fingerprint}\0{occurrence}");
        *occurrence += 1;
        if scope.claimed.contains_key(&key) {
            continue;
        }
        let Some(index) = scope.debts.iter().position(|debt| {
            at >= debt.since.saturating_sub(1_000)
                && debt
                    .before
                    .is_none_or(|before| before > debt.since && at < before)
        }) else {
            continue;
        };
        scope.debts.remove(index);
        scope.claimed.insert(key, now);
        counts.add(&usage);
    }
    counts
}

#[cfg(test)]
#[path = "../test/accounting.test.rs"]
mod tests;
