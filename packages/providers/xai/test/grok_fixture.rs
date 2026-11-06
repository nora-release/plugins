use nora_ai::{OAuthCredentials, OAuthProviderId};
use nora_provider_auth::grok_source::{ACCOUNT_ID_KEY, METADATA_KEY};
use serde_json::{json, Value};
use std::{collections::BTreeMap, path::PathBuf};

pub struct Fixture {
    pub directory: PathBuf,
    pub file: PathBuf,
    pub executable: PathBuf,
}

impl Fixture {
    pub fn new() -> Self {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let directory = std::env::temp_dir().join(format!(
            "nora-xai-renewal-{}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        Self {
            file: directory.join("auth.json"),
            executable: directory.join("grok"),
            directory,
        }
    }

    pub fn write(&self, token: &str, user: &str, expired: bool) {
        std::fs::write(&self.file, Self::record(token, user, expired).to_string()).unwrap();
    }

    pub fn record(token: &str, user: &str, expired: bool) -> Value {
        let expiry = if expired {
            "2000-01-01T00:00:00Z"
        } else {
            "2099-01-01T00:00:00Z"
        };
        json!({"https://auth.x.ai":{"key":token,"user_id":user,"oidc_issuer":"https://auth.x.ai","expires_at":expiry}})
    }

    pub fn credentials(&self) -> OAuthCredentials {
        OAuthCredentials::new(
            OAuthProviderId::XAI,
            "",
            None,
            None,
            BTreeMap::from([
                (ACCOUNT_ID_KEY.into(), json!("account-a").into()),
                ("userId".into(), json!("account-a").into()),
                (
                    METADATA_KEY.into(),
                    json!({
                        "kind":"grokCLI", "homeDirectory":self.directory,
                        "authFilePath":self.file, "executablePath":self.executable
                    })
                    .into(),
                ),
            ]),
        )
    }

    pub fn saved(&self) -> Value {
        nora_provider::auth::encode(&self.credentials(), "xai", true)
    }

    #[cfg(unix)]
    pub fn cli(&self, script: &str) {
        use std::os::unix::fs::PermissionsExt;
        std::fs::write(&self.executable, format!(
            "#!/bin/sh\nif [ \"$1\" = --version ]; then printf 'grok 99.0.0'; exit 0; fi\nprintf x >> \"$GROK_HOME/runs\"\n{script}\n"
        )).unwrap();
        std::fs::set_permissions(&self.executable, std::fs::Permissions::from_mode(0o700)).unwrap();
    }
}

impl Drop for Fixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}
