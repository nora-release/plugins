use super::*;
use std::path::{Path, PathBuf};

pub(super) struct Source {
    pub(super) kind: SourceKind,
    pub(super) path: PathBuf,
}

fn default_sources(
    home: &Path,
    environment: &BTreeMap<String, String>,
    platform: &str,
) -> Vec<Source> {
    let directory = |key: &str| {
        environment
            .get(key)
            .map(PathBuf::from)
            .filter(|path| path.is_absolute())
    };
    let cli = match platform {
        "macos" => home.join("Library/Application Support/kiro-cli/data.sqlite3"),
        "windows" => directory("APPDATA")
            .unwrap_or_else(|| home.join("AppData/Roaming"))
            .join("kiro-cli/data.sqlite3"),
        _ => directory("XDG_DATA_HOME")
            .unwrap_or_else(|| home.join(".local/share"))
            .join("kiro-cli/data.sqlite3"),
    };
    vec![
        Source {
            kind: SourceKind::Cli,
            path: cli,
        },
        Source {
            kind: SourceKind::Ide,
            path: home.join(".aws/sso/cache/kiro-auth-token.json"),
        },
    ]
}

pub(super) fn default_cli_path() -> Option<PathBuf> {
    let home = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))?;
    let home = PathBuf::from(home);
    if !home.is_absolute() {
        return None;
    }
    default_sources(&home, &std::env::vars().collect(), std::env::consts::OS)
        .into_iter()
        .find(|source| source.kind == SourceKind::Cli)
        .map(|source| source.path)
}

pub(super) async fn discover(provider: &Kiro, context: CallContext) -> PluginResult<Value> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
        .ok_or_else(|| PluginError::invalid("The current user's home directory is unavailable."))?;
    let mut sources = default_sources(&home, &std::env::vars().collect(), std::env::consts::OS);
    let mut source_error = false;
    match crate::magpie::default_path() {
        Ok(path) => match crate::magpie::list_sources(&path) {
            Ok(accounts) => sources.extend(accounts.into_iter().map(|account| Source {
                kind: SourceKind::Magpie(account),
                path: path.clone(),
            })),
            Err(_) => source_error = true,
        },
        Err(_) => source_error = true,
    }
    let client = provider.client(context.network_proxy.as_ref())?;
    let mut result = discover_sources(
        provider,
        &sources,
        client.as_ref(),
        context.network_proxy.as_ref(),
    )
    .await?;
    if source_error {
        if !result["errors"].is_array() {
            result["errors"] = json!([]);
        }
        result["errors"].as_array_mut().unwrap().push(json!({"sourceID":"magpieSource","message":"The saved Kiro sign-ins could not be read."}));
    }
    Ok(result)
}

pub(super) async fn discover_sources(
    provider: &Kiro,
    sources: &[Source],
    client: &dyn HttpClient,
    proxy: Option<&NetworkProxy>,
) -> PluginResult<Value> {
    let mut accounts = Vec::new();
    let mut errors = Vec::new();
    // Independent sign-in sources have their own locks and renew concurrently.
    let results = futures::future::join_all(sources.iter().map(|source| async {
        tokio::time::timeout(
            std::time::Duration::from_secs(50),
            provider.source_live(
                &source.kind,
                &source.path.to_string_lossy(),
                client,
                None,
                false,
                None,
                proxy,
            ),
        )
        .await
        .map_err(|_| {
            PluginError::transport("The local Kiro sign-in could not be verified in time.")
        })?
    }))
    .await;
    for (source, result) in sources.iter().zip(results) {
        match result {
            Ok(Some(credentials)) => accounts.push(json!({"sourceID":source.kind.auth_method(),"credentials":Kiro::source_saved(&credentials,&source.kind,&source.path.to_string_lossy())})),
            Ok(None) => {},
            Err(error) => {
                errors.push(json!({"sourceID":source.kind.auth_method(),"code":error.code,"message":error.message}));
            },
        }
    }
    let mut result = json!({"accounts":accounts});
    if !errors.is_empty() {
        result["errors"] = json!(errors);
    }
    Ok(result)
}

#[cfg(test)]
async fn discover_at(sources: &[Source], client: &dyn HttpClient) -> PluginResult<Value> {
    let provider = Kiro::default();
    *provider.config.write().unwrap() = Some(Config {
        proxy: None,
        methods: json!([]),
        installation: "fixture".into(),
    });
    discover_sources(&provider, sources, client, None).await
}

#[cfg(test)]
#[path = "../test/discovery.test.rs"]
mod tests;
