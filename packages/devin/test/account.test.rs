use super::*;
use crate::fixture::{self, Client, Reply};
#[test]
fn actual_server_identity_names_distinct_accounts() {
    let profile = identity(&fixture::me()).unwrap();
    assert_eq!(profile.email, "one@example.test");
    assert_eq!(profile.plan, "Devin Pro");
    assert!(identity(&json!({"userStatus":{}})).is_err());
}
#[test]
fn selected_source_reads_only_that_file_and_retains_no_token() {
    let path = fixture::temp("credentials.toml");
    let source =
        "windsurf_api_key = 'private-source-key'\napi_server_url = 'https://eu.example.test'\n";
    std::fs::write(&path, source).unwrap();
    let (key, server) = read(path.to_str().unwrap()).unwrap();
    assert_eq!(key, "private-source-key");
    assert_eq!(server, "https://eu.example.test");
    let mut account = Account::parse(&fixture::saved()).unwrap();
    account.source = Some(path.to_string_lossy().into());
    account.key = key;
    let saved = account.credentials();
    assert!(!saved.to_string().contains("private-source-key"));
    assert!(saved.get("key").is_none());
    assert_eq!(std::fs::read_to_string(&path).unwrap(), source);
    std::fs::remove_file(path).unwrap();
}
#[tokio::test]
async fn source_rebinding_to_a_different_account_is_rejected() {
    let path = fixture::temp("credentials.toml");
    std::fs::write(&path, "windsurf_api_key='new-private-key'\n").unwrap();
    let mut value = fixture::saved();
    value.as_object_mut().unwrap().remove("key");
    value["source"] = json!({"path":path});
    let mut who = fixture::me();
    who["userStatus"]["email"] = json!("other@example.test");
    let client = Client::new(vec![Reply::json(who)]);
    assert!(Account::live(&value, client.as_ref()).await.is_err());
    std::fs::remove_file(path).unwrap();
}
#[test]
fn plain_http_credential_urls_and_extra_paths_are_never_used() {
    for server in [
        "http://unsafe.test",
        "https://user:private@host.test",
        "https://host.test/path",
        "https://host.test?private",
    ] {
        assert!(super::server(server).is_err());
    }
    assert!(read("relative/credentials.toml").is_err());
}
#[cfg(unix)]
#[test]
fn selected_source_cannot_be_a_symlink_or_directory() {
    let original = fixture::temp("original.toml");
    let link = fixture::temp("symlink.toml");
    std::fs::write(&original, "windsurf_api_key='private'\n").unwrap();
    std::os::unix::fs::symlink(&original, &link).unwrap();
    assert!(read(link.to_str().unwrap()).is_err());
    assert!(read(std::env::temp_dir().to_str().unwrap()).is_err());
    std::fs::remove_file(link).unwrap();
    std::fs::remove_file(original).unwrap();
}
