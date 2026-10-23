//! Optional desktop device identity; read only after the user selects it.
//! Codec adapted from Magpie and dsh-connect-trae (MIT).
use crate::*;
use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, KeyIvInit};
use base64::{engine::general_purpose::STANDARD, Engine};
use sha2::{Digest, Sha512};
use std::path::{Path, PathBuf};
use tokio::io::AsyncReadExt;
const DC: &str = "iCubeAuthInfo://icube-dc:";
const AUTH: &str = "iCubeAuthInfo://icube.cloudide";
const SALT_0:&str="4dd4c2e6b83162090e52b3c7a6733ba41cb2462b829ab58a196b39db57177524f49baf7f08e8d68d26a72e37c1a95a2f1f05a51892aef2949732b62a38aadd58";
const SALT_1:&str="490cc21292b05d0cc06ccbe91fb92e21b5a7c538811dc85e27a96e28104e1cf69c7166efab232807390da78a8b91e49c44b49a43feb941a1e46f2656e613e47b";

pub(crate) fn decode(value: &str) -> PluginResult<String> {
    let bytes = STANDARD
        .decode(value)
        .map_err(|_| invalid("Trae's desktop sign-in value is not readable."))?;
    if bytes.len() <= 102 || bytes.len() > 1024 * 1024 {
        return Err(invalid("Trae's desktop sign-in value has an invalid size."));
    }
    let prefix = bytes[..6]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let salt = match prefix.as_str() {
        "746305100000" => SALT_0,
        "123920200203" => SALT_1,
        _ => return Err(invalid("This Trae desktop sign-in codec is unsupported.")),
    };
    let salt = (0..salt.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&salt[index..index + 2], 16).unwrap())
        .collect::<Vec<_>>();
    let mut input = Sha512::digest(&bytes[6..38]).to_vec();
    input.extend(salt);
    let key = Sha512::digest(input);
    let cipher = cbc::Decryptor::<aes::Aes128>::new_from_slices(&key[..16], &key[16..32])
        .map_err(|_| invalid("Trae's desktop device key is invalid."))?;
    let clear = cipher
        .decrypt_padded_vec_mut::<Pkcs7>(&bytes[38..])
        .map_err(|_| invalid("Trae's desktop device value is invalid."))?;
    if clear.len() < 64 || Sha512::digest(&clear[64..]).as_slice() != &clear[..64] {
        return Err(invalid(
            "Trae's desktop device value did not pass its integrity check.",
        ));
    }
    String::from_utf8(clear[64..].to_vec())
        .map_err(|_| invalid("Trae's desktop device value is not valid text."))
}
async fn read(path: &Path, uid: &str) -> PluginResult<String> {
    let file = tokio::fs::File::open(path)
        .await
        .map_err(|_| invalid("The selected Trae desktop storage file is unavailable."))?;
    let metadata = file
        .metadata()
        .await
        .map_err(|_| invalid("The selected Trae desktop storage file cannot be inspected."))?;
    if !metadata.is_file() || metadata.len() > 1024 * 1024 {
        return Err(invalid(
            "The selected Trae desktop storage file is invalid or too large.",
        ));
    }
    let mut bytes = Vec::new();
    file.take(1024 * 1024 + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| invalid("The Trae desktop storage file could not be read."))?;
    if bytes.len() > 1024 * 1024 {
        return Err(invalid("The Trae desktop storage file is too large."));
    }
    let value: Value = serde_json::from_slice(&bytes)
        .map_err(|_| invalid("The Trae desktop storage file is invalid JSON."))?;
    let document = value
        .as_object()
        .ok_or_else(|| invalid("Trae's desktop device identity is unavailable."))?;
    let ids = document
        .keys()
        .filter_map(|key| key.strip_prefix(DC))
        .collect::<Vec<_>>();
    if ids.len() != 1
        || !(8..=20).contains(&ids[0].len())
        || !ids[0].bytes().all(|byte| byte.is_ascii_digit())
    {
        return Err(invalid(
            "Trae's desktop storage does not identify one device.",
        ));
    }
    let raw = value
        .get(AUTH)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| invalid("Sign in to this account in Trae CN before using its device."))?;
    let clear = if raw.trim_start().starts_with('{') {
        raw.to_owned()
    } else {
        decode(raw.trim())?
    };
    let profile: Value = serde_json::from_str(&clear)
        .map_err(|_| invalid("Trae's desktop account identity is invalid."))?;
    if profile.get("userId").map(identifier).as_deref() != Some(uid) {
        return Err(invalid("This Trae CN installation is signed in to a different account. Use the original account for daily credits."));
    }
    Ok(ids[0].into())
}
fn paths() -> PluginResult<Vec<PathBuf>> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| invalid("The Trae desktop home directory is unavailable."))?;
    let file = |root: PathBuf, name: &str| root.join(name).join("User/globalStorage/storage.json");
    match std::env::consts::OS {
        "macos" => Ok(vec![file(
            home.join("Library/Application Support"),
            "Trae CN",
        )]),
        "windows" => {
            let mut roots = vec![home.join("AppData/Roaming")];
            if let Some(root) = std::env::var_os("APPDATA") {
                roots.insert(0, root.into());
            }
            let mut paths = Vec::new();
            for root in roots {
                for name in ["Trae CN", "trae-cn"] {
                    let path = file(root.clone(), name);
                    if !paths.iter().any(|existing: &PathBuf| {
                        existing.to_string_lossy().to_lowercase()
                            == path.to_string_lossy().to_lowercase()
                    }) {
                        paths.push(path);
                    }
                }
            }
            Ok(paths)
        }
        "linux" => {
            let root = std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"));
            Ok(vec![file(root.clone(), "trae-cn"), file(root, "Trae CN")])
        }
        _ => Err(invalid(
            "Trae desktop device discovery is unavailable on this platform.",
        )),
    }
}
pub(crate) async fn mark(credentials: &mut Value, input: &str) -> PluginResult<()> {
    let path = if !input.trim().is_empty() {
        let path = PathBuf::from(input.trim());
        if !path.is_absolute() {
            return Err(invalid("Choose an absolute Trae CN storage file path."));
        }
        path
    } else {
        let mut found = None;
        for candidate in paths()? {
            if tokio::fs::metadata(&candidate).await.is_ok() {
                found = Some(candidate);
                break;
            }
        }
        found.ok_or_else(||invalid("Trae CN desktop storage was not found. Choose its storage file or use browser sign-in."))?
    };
    read(&path, text(credentials, "uid")).await?;
    credentials["desktopDevice"] = json!({"path":path,"uid":credentials.get("uid")});
    Ok(())
}
pub(crate) async fn device(credentials: &Value) -> PluginResult<Option<String>> {
    let Some(marker) = credentials.get("desktopDevice") else {
        return Ok(None);
    };
    if text(marker, "uid") != text(credentials, "uid") {
        return Err(invalid(
            "The selected Trae desktop device belongs to another account.",
        ));
    }
    let path = PathBuf::from(text(marker, "path"));
    if !path.is_absolute() {
        return Err(invalid(
            "The selected Trae desktop storage path is invalid.",
        ));
    }
    read(&path, text(credentials, "uid")).await.map(Some)
}
pub(crate) fn machine() -> PluginResult<(String, String)> {
    #[cfg(unix)]
    {
        let mut name = std::mem::MaybeUninit::<libc::utsname>::zeroed();
        if unsafe { libc::uname(name.as_mut_ptr()) } != 0 {
            return Err(transport("This machine's OS version is unavailable."));
        }
        let name = unsafe { name.assume_init() };
        let release = unsafe { std::ffi::CStr::from_ptr(name.release.as_ptr()) }.to_string_lossy();
        let platform = if cfg!(target_os = "macos") {
            ("mac", "macOS")
        } else {
            (std::env::consts::OS, std::env::consts::OS)
        };
        return Ok((platform.0.into(), format!("{} {release}", platform.1)));
    }
    #[cfg(not(unix))]
    {
        Ok(("windows".into(), "Windows 10.0.22631".into()))
    }
}
#[cfg(test)]
#[path = "../test/desktop.test.rs"]
mod tests;
