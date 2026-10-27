use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};

const PREFERENCE_TTL: Duration = Duration::from_secs(30 * 60);

#[derive(Default)]
pub(super) struct Preferences(HashMap<String, Instant>);

impl Preferences {
    pub(super) fn contains(&mut self, key: &str, now: Instant) -> bool {
        let Some(since) = self.0.get(key) else {
            return false;
        };
        if now.saturating_duration_since(*since) <= PREFERENCE_TTL {
            return true;
        }
        self.0.remove(key);
        false
    }

    pub(super) fn remember(&mut self, key: String, now: Instant) {
        self.0
            .retain(|_, since| now.saturating_duration_since(*since) <= PREFERENCE_TTL);
        self.0.insert(key, now);
    }
}

pub(super) fn preferences() -> &'static Mutex<Preferences> {
    static PREFERENCES: OnceLock<Mutex<Preferences>> = OnceLock::new();
    PREFERENCES.get_or_init(Mutex::default)
}

#[cfg(test)]
#[path = "../../test/ipv4_preference.test.rs"]
mod tests;
