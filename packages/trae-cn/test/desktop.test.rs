use super::*;
use aes::cipher::BlockEncryptMut;
#[test]
fn actual_client_codec_checks_its_sha512_prefix_before_reading_account_identity() {
    let random = [7u8; 32];
    let salt = (0..SALT_0.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&SALT_0[index..index + 2], 16).unwrap())
        .collect::<Vec<_>>();
    let mut input = Sha512::digest(random).to_vec();
    input.extend(salt);
    let key = Sha512::digest(input);
    let text = r#"{"userId":"member","token":"must-not-be-imported"}"#;
    let mut clear = Sha512::digest(text.as_bytes()).to_vec();
    clear.extend(text.as_bytes());
    let cipher = cbc::Encryptor::<aes::Aes128>::new_from_slices(&key[..16], &key[16..32]).unwrap();
    let encrypted = cipher.encrypt_padded_vec_mut::<Pkcs7>(&clear);
    let mut bytes = vec![0x74, 0x63, 0x05, 0x10, 0, 0];
    bytes.extend(random);
    bytes.extend(encrypted);
    assert_eq!(decode(&STANDARD.encode(&bytes)).unwrap(), text);
    bytes[50] ^= 1;
    assert!(decode(&STANDARD.encode(bytes)).is_err());
}
#[tokio::test]
async fn desktop_device_is_explicit_read_only_and_stops_if_the_installation_changes_accounts() {
    let directory = std::env::temp_dir().join(format!("trae-device-{}", id()));
    std::fs::create_dir_all(&directory).unwrap();
    let path = directory.join("storage.json");
    let initial=json!({"iCubeAuthInfo://icube-dc:1234567890123456":{},"iCubeAuthInfo://icube.cloudide":json!({"userId":"member","token":"desktop-secret"}).to_string()}).to_string();
    std::fs::write(&path, &initial).unwrap();
    let mut credentials = crate::tests::credentials();
    assert_eq!(device(&credentials).await.unwrap(), None);
    mark(&mut credentials, path.to_str().unwrap())
        .await
        .unwrap();
    assert_eq!(
        device(&credentials).await.unwrap(),
        Some("1234567890123456".into())
    );
    assert_eq!(credentials["access"], "jwt-one");
    assert!(!credentials.to_string().contains("desktop-secret"));
    assert_eq!(std::fs::read_to_string(&path).unwrap(), initial);
    std::fs::write(&path,json!({"iCubeAuthInfo://icube-dc:1234567890123456":{},"iCubeAuthInfo://icube.cloudide":json!({"userId":"another-account"}).to_string()}).to_string()).unwrap();
    assert!(device(&credentials).await.is_err());
    std::fs::remove_dir_all(directory).unwrap();
}
