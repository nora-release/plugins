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

impl SourceState {
    fn resume(&mut self) -> PluginResult<Option<SourceSession>> {
        let Some(pending) = self.pending.as_mut() else {
            return Ok(None);
        };
        match pending.session.state(&pending.next)? {
            DiskState::Changed | DiskState::SignedOut => {
                self.pending = None;
                self.verified = None;
                return Ok(None);
            }
            DiskState::Original => pending.session.replace_refreshed(&pending.next)?,
            DiskState::Written => pending.session.reconcile_refreshed(&pending.next)?,
        }
        // Only an unwritten rotation is pending. Once durable, the source can
        // be renewed again even if its optional identity service stays down.
        Ok(self.pending.take().map(|pending| pending.session))
    }

    fn persist(&mut self, session: SourceSession, next: Credential) -> PluginResult<SourceSession> {
        // Keep the rotated token before any fallible write or further await.
        self.pending = Some(Renewal { session, next });
        self.resume()?.ok_or_else(session::expired)
    }
}

fn fresh(credential: &Credential) -> bool {
    credential.expires <= 0 || credential.fresh()
}

fn key(credential: &Credential, expected: Option<&Value>) -> String {
    session::fingerprint(&format!(
        "{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
        credential.access,
        credential.refresh,
        credential.profile,
        credential.realm,
        credential.method,
        credential.login_provider,
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
        let mut force = force;
        let mut rejected = rejected.map(str::to_owned);
        let mut retry_source = None;
        let mut renewed = false;
        loop {
            let opened = match state.resume()? {
                Some(source) => Some(source),
                None => open(kind, path).await?,
            };
            let Some(mut source) = opened else {
                return if retry_source.is_some() {
                    Err(session::expired())
                } else {
                    Ok(None)
                };
            };
            let mut credential = source.credential().clone();
            if retry_source
                .take()
                .is_some_and(|previous| previous != key(&credential, None))
            {
                return Err(session::expired());
            }
            source.select_identity(expected)?;
            let rejected_current = force
                && rejected
                    .as_deref()
                    .is_none_or(|access| access == credential.access);
            if !fresh(&credential) || rejected_current {
                renewed = true;
                state.verified = None;
                let original_access = credential.access.clone();
                if *kind == SourceKind::Cli && self.owner.owns(Path::new(&selected)) {
                    // The owner may save before returning an error. Always reread it.
                    let _ = self.owner.renew(proxy.as_ref()).await;
                    credential = source.reload()?.ok_or_else(session::expired)?;
                }
                if !fresh(&credential) || (rejected_current && credential.access == original_access)
                {
                    let next = network::refresh(client, &credential).await?;
                    source = state.persist(source, next)?;
                    credential = source.credential().clone();
                }
            }
            match verify(
                &mut source,
                &credential,
                client,
                expected,
                state.verified.as_ref(),
            )
            .await
            {
                Ok((fingerprint, credential)) => {
                    state.verified = Some((fingerprint, credential.clone()));
                    return Ok(Some(credential));
                }
                Err(error)
                    if !renewed
                        && credential.method != "apikey"
                        && network::authentication_rejected(&error) =>
                {
                    // Reopen under the source lock before renewing. A source that
                    // switched while unlocked must never be renewed as this account.
                    retry_source = Some(key(&credential, None));
                    rejected = Some(credential.access);
                    force = true;
                    state.verified = None;
                }
                Err(error) => return Err(error),
            }
        }
    }
}

async fn open(kind: &SourceKind, path: &str) -> PluginResult<Option<SourceSession>> {
    let kind = kind.clone();
    let path = path.to_owned();
    tokio::task::spawn_blocking(move || {
        SourceSession::open(&kind, &path, &crate::source::lock_root())
    })
    .await
    .map_err(|_| session::expired())?
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
    let identified = if let Some((_, cached)) = cached.filter(|(key, _)| key == &fingerprint) {
        credential = cached.clone();
        Ok(())
    } else {
        // Identity lookup can fill a profile or fail after doing so. Keep the
        // durable token unchanged for source checks and renewal retries.
        network::identify(client, &mut credential).await.map(|_| ())
    };
    if source.state(durable)? != DiskState::Written {
        return Err(session::expired());
    }
    identified?;
    if credential.account_identity().is_none()
        || expected.is_some_and(|saved| !credential.matches_saved_identity(saved))
    {
        return Err(session::expired());
    }
    Ok((fingerprint, credential))
}

#[cfg(test)]
#[path = "../test/source_runtime.test.rs"]
mod tests;
