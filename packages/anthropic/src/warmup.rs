use super::*;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWriteExt};

struct Work(PathBuf);
impl Drop for Work {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
async fn limited(reader: impl AsyncRead + Unpin, limit: usize) -> PluginResult<Vec<u8>> {
    let mut bytes = Vec::new();
    reader
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| PluginError::transport("Claude Code warm-up output could not be read."))?;
    if bytes.len() > limit {
        return Err(PluginError::transport(
            "Claude Code warm-up output exceeded its limit.",
        ));
    }
    Ok(bytes)
}
impl Claude {
    pub(super) async fn warmup(&self, params: &Value) -> PluginResult<Value> {
        if params["confirmed"] != true {
            return Err(PluginError::invalid(
                "Confirm the saved account before warming its subscription allowance.",
            ));
        }
        nora_provider_plugin_sdk::require_subscription_accounts()?;
        if params["subscriptionAccountsEnabled"] == false {
            return Err(PluginError::unsupported(
                "subscription warm-up while subscriptions are disabled",
            ));
        }
        let id = cli::text(params.get("account"), 128)
            .ok_or_else(|| PluginError::invalid("Choose a saved Claude account."))?;
        let value = params
            .get("credentials")
            .filter(|v| !is_api(v))
            .ok_or_else(|| PluginError::authentication("Connect a Claude subscription account."))?;
        tokio::time::timeout(std::time::Duration::from_secs(120), async {
            let proxy = self.params_proxy(params).await?;
            let mut account = self.account(id, value, proxy.clone()).await?;
            let work = Work(
                self.config()?
                    .root
                    .join(format!("warmup-{}", nora_ai::oauth_pkce::random_hex(16))),
            );
            cli::directory(&work.0)?;
            let args = [
                "-p",
                "--output-format",
                "json",
                "--model",
                "haiku",
                "--tools",
                "",
                "--strict-mcp-config",
                "--setting-sources",
                "",
                "--no-session-persistence",
            ]
            .map(str::to_owned);
            let mut process = cli::Process::spawn(
                &account.executable,
                &args,
                &work.0,
                cli::env(&account.home, &proxy, false),
            )?;
            let mut input = process.child.stdin.take().ok_or_else(cli::error)?;
            let stdout = process.child.stdout.take().ok_or_else(cli::error)?;
            let stderr = process.child.stderr.take().ok_or_else(cli::error)?;
            input.write_all(b"hi").await.map_err(|_| {
                PluginError::transport("Claude Code warm-up input could not be sent.")
            })?;
            drop(input);
            let (out, err) =
                tokio::try_join!(limited(stdout, 1024 * 1024), limited(stderr, 64 * 1024))?;
            let status = process
                .child
                .wait()
                .await
                .map_err(|_| PluginError::transport("Claude Code warm-up did not finish."))?;
            let output = std::process::Output {
                status,
                stdout: out,
                stderr: err,
            };
            let result = cli_output::result(&output.stdout);
            if !output.status.success() || result.as_ref().is_none_or(|v| v["is_error"] == true) {
                return Err(cli_output::failure(&output, &account.home).await);
            }
            stream::synchronize(&mut account, self.persistence.as_ref()).await?;
            self.usage.lock().await.remove(id);
            Ok(json!({"outcome":"warmed"}))
        })
        .await
        .map_err(|_| PluginError::transport("Claude Code warm-up timed out."))?
    }
}
