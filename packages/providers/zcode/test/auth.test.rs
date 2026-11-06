use super::*;
fn encrypted(key: &[u8; 32], text: &str) -> Value {
    let nonce = [7u8; 12];
    let cipher = Aes256Gcm::new_from_slice(key).unwrap();
    let mut body = cipher
        .encrypt(Nonce::from_slice(&nonce), text.as_bytes())
        .unwrap();
    let tag = body.split_off(body.len() - 16);
    let encode = |bytes: &[u8]| base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    json!(format!(
        "enc:v1:{}.{}.{}",
        encode(&nonce),
        encode(&tag),
        encode(&body)
    ))
}
#[test]
fn aes_gcm_credentials_use_the_real_iv_tag_data_format_and_reject_tampering() {
    let key = secret(Some("explicit-test-secret"));
    let value = encrypted(&key, "actual-key.secret");
    assert_eq!(decrypt(&key, &value).as_deref(), Some("actual-key.secret"));
    assert!(decrypt(&secret(Some("other")), &value).is_none());
    assert!(decrypt(&key, &json!("enc:v1:bad.parts")).is_none());
    assert!(decrypt(&key, &json!("plain-key")).is_none());
}
#[test]
fn explicitly_selected_sources_are_read_only_and_bound_to_the_selected_key() {
    let path = std::env::temp_dir().join(format!("zcode-source-{}.json", uuid::Uuid::new_v4()));
    let key = secret(Some("explicit-test-secret"));
    let value = json!({"account-provider:coding-plan:account:zai-individual-coding-plan:account:one:api-key":encrypted(&key,"key.secret"),"oauth:zai:user_info":encrypted(&key,r#"{"email":"person@example.com"}"#)});
    let bytes = serde_json::to_vec(&value).unwrap();
    std::fs::write(&path, &bytes).unwrap();
    let state = source(
        &json!({"sourcePath":path,"credentialSecret":"explicit-test-secret"}),
        "device",
    )
    .unwrap();
    assert_eq!(state["key"], "key.secret");
    assert_eq!(state["uid"], core::fingerprint(&json!("key.secret")));
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    std::fs::remove_file(path).unwrap();
}
#[test]
fn current_config_key_can_be_selected_without_modifying_app_credentials() {
    let directory = std::env::temp_dir().join(format!("zcode-source-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir(&directory).unwrap();
    let credentials = directory.join("credentials.json");
    let config = directory.join("config.json");
    std::fs::write(&credentials, b"{}").unwrap();
    std::fs::write(&config,serde_json::to_vec(&json!({"provider":{"account:bigmodel-individual-coding-plan":{"options":{"apiKey":"current.secret"}}}})).unwrap()).unwrap();
    let state = source(
        &json!({"sourcePath":credentials,"configPath":config}),
        "device",
    )
    .unwrap();
    assert_eq!(state["site"], "bigmodel");
    assert_eq!(state["key"], "current.secret");
    assert_eq!(std::fs::read(&credentials).unwrap(), b"{}");
    std::fs::remove_dir_all(directory).unwrap();
}
#[cfg(unix)]
#[test]
fn source_files_reject_relative_paths_symlinks_and_oversized_content() {
    assert!(read_json("relative.json").is_err());
    let path = std::env::temp_dir().join(format!("zcode-source-{}", uuid::Uuid::new_v4()));
    let link = path.with_extension("link");
    std::fs::write(&path, b"{}").unwrap();
    std::os::unix::fs::symlink(&path, &link).unwrap();
    assert!(read_json(link.to_str().unwrap()).is_err());
    std::fs::remove_file(link).unwrap();
    std::fs::write(&path, vec![b' '; 2 * 1024 * 1024 + 1]).unwrap();
    assert!(read_json(path.to_str().unwrap()).is_err());
    std::fs::remove_file(path).unwrap();
}
