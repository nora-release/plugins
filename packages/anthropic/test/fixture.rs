use serde_json::{json, Value};
use std::{
    path::{Path, PathBuf},
    sync::OnceLock,
};
pub struct Directory(pub PathBuf);
impl Directory {
    pub fn new() -> Self {
        let path = std::env::temp_dir().join(format!(
            "nora-claude-fixture-{}",
            nora_ai::oauth_pkce::random_hex(12)
        ));
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
pub fn binary() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        let deps = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let rlib = std::fs::read_dir(&deps)
            .unwrap()
            .flatten()
            .map(|entry| entry.path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("libserde_json-")
                    && path
                        .extension()
                        .is_some_and(|extension| extension == "rlib")
            })
            .max_by_key(|path| std::fs::metadata(path).unwrap().modified().unwrap())
            .expect("serde_json test dependency");
        let output = deps.join(format!(
            "nora-fake-claude-{}{}",
            std::process::id(),
            std::env::consts::EXE_SUFFIX
        ));
        let source = Path::new(env!("CARGO_MANIFEST_DIR")).join("test/fake-cli.rs");
        let status =
            std::process::Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
                .arg("--edition=2021")
                .arg(source)
                .arg("--crate-name=nora_fake_claude")
                .arg("--extern")
                .arg(format!("serde_json={}", rlib.display()))
                .arg("-L")
                .arg(format!("dependency={}", deps.display()))
                .arg("-o")
                .arg(&output)
                .status()
                .unwrap();
        assert!(status.success());
        output
    })
    .clone()
}
pub fn profile(directory: &Path) -> Value {
    profile_with_identity(directory, "account-a")
}
pub fn profile_with_identity(directory: &Path, account_id: &str) -> Value {
    std::fs::create_dir_all(directory).unwrap();
    std::fs::write(directory.join(".credentials.json"),json!({"claudeAiOauth":{"accessToken":"fixture-access","refreshToken":"fixture-refresh","expiresAt":4102444800000i64,"scopes":["user:inference","user:profile"],"subscriptionType":"max"}}).to_string()).unwrap();
    std::fs::write(directory.join(".claude.json"),json!({"oauthAccount":{"emailAddress":"person@example.test","accountUuid":account_id,"organizationUuid":"org-a","organizationName":"Example"}}).to_string()).unwrap();
    let id = {
        use sha2::{Digest, Sha256};
        format!(
            "{:x}",
            Sha256::digest(format!("claude-account\0{account_id}\0org-a").as_bytes())
        )
    };
    json!({"type":"oauth","provider":"anthropic","credentialProvider":"anthropic","access":"fixture-access","refresh":"fixture-refresh","expires":4102444800000i64,"accountId":id,"email":"person@example.test","plan":"max","extras":{"noraClaudeCLI":{"directory":directory,"executable":binary(),"identity":id,"organization":"Example"}}})
}
