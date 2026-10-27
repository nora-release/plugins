#![allow(clippy::bool_assert_comparison)]

pub use async_trait::async_trait;
pub use nora_ai::oauth::*;
pub use nora_ai::*;
pub use nora_provider_auth::*;
pub use serde_json::{json, Map, Value};
pub use std::collections::{BTreeMap, BTreeSet};
pub use std::env;
pub use std::fs;
pub use std::path::{Path, PathBuf};
pub use std::sync::{Arc, Mutex};
pub use std::time::Duration;
pub use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const ANTHROPIC_CLIENT_ID: &str = "9d1c250a-e61b-44d9-88ed-5944d1962f5e";
pub const OPENAI_CODEX_CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
pub const EARLY_REFRESH_MILLIS: i64 = 5 * 60 * 1_000;

mod oauth_test {
    include!("oauth.test.rs");
}

mod oauth_callback_server_test {
    include!("oauth_callback_server.test.rs");
}

mod oauth_login_test {
    include!("oauth_login.test.rs");
}

mod oauth_providers_test {
    include!("oauth_providers.test.rs");
}

mod xai_auth_test {
    include!("xai_auth.test.rs");
}

mod subscription_policy_test {
    include!("subscription_policy.test.rs");
}

mod callback_input_test {
    include!("callback_input.test.rs");
}
