use super::*;
use crate::source::{SourceSession, SourceState as DiskState};
use std::{path::Path, time::Duration};

#[derive(Default)]
pub(super) struct SourceState {
    pending: Option<Renewal>,
    verified: Option<(String, Credential)>,
}

struct Renewal {
    session: SourceSession,
    next: Credential,
}

fn fresh(credential: &Credential) -> bool {
    credential.expires <= 0 || credential.fresh()
}

fn key(credential: &Credential, expected: Option<&Value>) -> String {
    session::fingerprint(&format!(
        "{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
        credential.access,
        credential.refresh,
        credential.profile,
        credential.realm,
        credential.method,
        credential.region,
        credential.client_id,
        credential.client_secret,
        credential.token_endpoint,
        expected.unwrap_or(&Value::Null)
    ))
}

impl Kiro {
    pub(super) async fn source_live(
        &self,
        kind: &SourceKind,
        path: &str,
        client: &dyn HttpClient,
        expected: Option<&Value>,
        force: bool,
        rejected: Option<&str>,
        proxy: Option<&NetworkProxy>,
    ) -> PluginResult<Option<Credential>> {
        let proxy = proxy.cloned().or(self.config()?.proxy);
        if let Some(proxy) = &proxy {
            proxy.validate()?;
        }
        let selected = std::path::PathBuf::from(path);
        if !selected.is_absolute() {
            return Err(session::expired());
        }
        let canonical = std::fs::canonicalize(&selected).unwrap_or_else(|_| selected.clone());
        let source_key = session::fingerprint(&format!(
            "{}\0{}",
            kind.cache_key(),
            canonical.to_string_lossy()
        ));
        let slot = {
            let mut sources = self.sources.lock().await;
            if !sources.contains_key(&source_key) && sources.len() >= 64 {
                return Err(PluginError::transport(
                    "Too many local Kiro sign-ins are being renewed.",
                ));
            }
            sources
                .entry(source_key)
                .or_insert_with(|| Arc::new(Mutex::new(SourceState::default())))
                .clone()
        };
        let mut state = tokio::time::timeout(Duration::from_secs(40), slot.lock())
            .await
            .map_err(|_| {
                PluginError::transport("The local Kiro sign-in is busy. Try again shortly.")
            })?;
        if let Some(pending) = state.pending.as_ref() {
            match pending.session.state(&pending.next)? {
                DiskState::Changed | DiskState::SignedOut => {
                    state.pending = None;
                    state.verified = None;
                }
                DiskState::Original | DiskState::Written => {}
            }
        }
        if state.pending.is_some() {
            return finish(&mut state, client, expected).await.map(Some);
        }
        let path = path.to_owned();
        let selected_kind = kind.clone();
        let opened = tokio::task::spawn_blocking(move || {
            SourceSession::open(&selected_kind, &path, &crate::source::lock_root())
        })
        .await
        .map_err(|_| session::expired())??;
        let Some(mut source) = opened else {
            return Ok(None);
        };
        if let Some(expected) = expected {
            source.bind_identity(expected)?;
        }
        let mut credential = source.credential().clone();
        let original_access = credential.access.clone();
        let renew = !fresh(&credential)
            || (force && rejected.is_none_or(|access| access == credential.access));
        if renew && *kind == SourceKind::Cli && self.owner.owns(Path::new(&selected)) {
            // The owner may successfully save before returning an error. Always reread it.
            let _ = self.owner.renew(proxy.as_ref()).await;
            credential = source.reload()?.ok_or_else(session::expired)?;
            if let Some(expected) = expected {
                source.bind_identity(expected)?;
            }
        }
        let owner_changed = credential.access != original_access;
        let renew = !fresh(&credential)
            || (force
                && !owner_changed
                && rejected.is_none_or(|access| access == credential.access));
        if renew {
            let next = network::refresh(client, &credential).await?;
            // Keep both the source lock and the rotated token before any further await.
            // A later RPC can retry verification/save without spending the old refresh token.
            state.pending = Some(Renewal {
                session: source,
                next,
            });
            return finish(&mut state, client, expected).await.map(Some);
        }
        let (fingerprint, credential) = verify(
            &mut source,
            &credential,
            client,
            expected,
            state.verified.as_ref(),
        )
        .await?;
        state.verified = Some((fingerprint, credential.clone()));
        Ok(Some(credential))
    }
}

async fn finish(
    state: &mut SourceState,
    client: &dyn HttpClient,
    requested: Option<&Value>,
) -> PluginResult<Credential> {
    let pending = state.pending.as_mut().ok_or_else(session::expired)?;
    let written = match pending.session.state(&pending.next)? {
        DiskState::Original => false,
        DiskState::Written => true,
        DiskState::Changed | DiskState::SignedOut => return Err(session::expired()),
    };
    if !written {
        // Kiro has already rotated the original source's token. Persist it
        // before optional profile services can fail or the process exits.
        pending.session.replace_refreshed(&pending.next)?;
    }
    let (fingerprint, credential) = verify(
        &mut pending.session,
        &pending.next,
        client,
        requested,
        state.verified.as_ref(),
    )
    .await?;
    state.pending = None;
    state.verified = Some((fingerprint, credential.clone()));
    Ok(credential)
}

async fn verify(
    source: &mut SourceSession,
    durable: &Credential,
    client: &dyn HttpClient,
    expected: Option<&Value>,
    cached: Option<&(String, Credential)>,
) -> PluginResult<(String, Credential)> {
    source.release_for_verification(durable)?;
    let fingerprint = key(durable, expected);
    let mut credential = durable.clone();
    if let Some((_, cached)) = cached.filter(|(key, _)| key == &fingerprint) {
        credential = cached.clone();
    } else {
        // Identity lookup can fill a profile or fail after doing so. Keep the
        // durable token unchanged for source checks and renewal retries.
        network::identify(client, &mut credential).await?;
    }
    if source.state(durable)? != DiskState::Written
        || credential.account_identity().is_none()
        || expected.is_some_and(|saved| !credential.matches_saved_identity(saved))
    {
        return Err(session::expired());
    }
    Ok((fingerprint, credential))
}

#[cfg(test)]
#[path = "../test/source_runtime.test.rs"]
mod tests;
