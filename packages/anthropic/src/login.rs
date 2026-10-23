use super::{cli, proxy::Proxy};
use nora_provider_plugin_sdk::{PluginError, PluginResult};
use serde_json::Value;
use std::{path::Path, sync::Arc};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    sync::{mpsc, Mutex},
};
pub(crate) struct Pending {
    task: Option<tokio::task::JoinHandle<PluginResult<cli::Account>>>,
    input: Arc<Mutex<Option<tokio::process::ChildStdin>>>,
}
impl Drop for Pending {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}
impl Pending {
    pub async fn finish(mut self, params: &Value) -> PluginResult<cli::Account> {
        if let Some(code) = cli::text(
            params.get("code").or_else(|| params.get("response")),
            64 * 1024,
        ) {
            if code
                .chars()
                .any(|character| character == '\r' || character == '\n')
            {
                return Err(PluginError::invalid(
                    "Paste the single code returned by Claude Code.",
                ));
            }
            if let Some(input) = self.input.lock().await.as_mut() {
                input
                    .write_all(format!("{code}\n").as_bytes())
                    .await
                    .map_err(|_| cli::error())?;
                input.flush().await.map_err(|_| cli::error())?;
            }
        }
        self.task
            .take()
            .ok_or_else(cli::error)?
            .await
            .map_err(|_| cli::error())?
    }
}
async fn read(
    mut output: impl tokio::io::AsyncRead + Unpin + Send + 'static,
    send: mpsc::Sender<String>,
) {
    let mut buffer = String::new();
    let mut sent = false;
    loop {
        let mut chunk = [0; 4096];
        let Ok(size) = output.read(&mut chunk).await else {
            break;
        };
        if size == 0 {
            break;
        }
        if sent {
            continue;
        }
        buffer.push_str(&String::from_utf8_lossy(&chunk[..size]));
        for word in buffer.split(|character: char| {
            character.is_whitespace() || character == '\x1b' || character == '"'
        }) {
            let candidate = word.trim_matches(['\'', ')', '(', '[', ']']);
            if let Ok(url) = url::Url::parse(candidate) {
                if cli::login_url(&url) {
                    let _ = send.send(url.to_string()).await;
                    sent = true;
                    break;
                }
            }
        }
        if buffer.len() > 64 * 1024 {
            buffer = buffer
                .chars()
                .rev()
                .take(32768)
                .collect::<String>()
                .chars()
                .rev()
                .collect();
        }
    }
}
pub(crate) async fn start(
    root: &Path,
    selected: Option<&str>,
    proxy: Arc<Proxy>,
) -> PluginResult<(Pending, String)> {
    let executable = cli::executable(selected)?;
    let directory = root.join(format!("signin-{}", cli::random()));
    cli::directory(&directory)?;
    let opened = directory.join(".opened-url");
    let mut environment = cli::env(&cli::Home::Isolated(directory.clone()), &proxy, false);
    environment.insert(
        "BROWSER".into(),
        std::env::current_exe()
            .map_err(|_| cli::error())?
            .to_string_lossy()
            .into(),
    );
    environment.insert(
        "NORA_CLAUDE_OPENED_URL".into(),
        opened.to_string_lossy().into(),
    );
    let mut process = cli::Process::spawn(
        &executable,
        &["auth".into(), "login".into(), "--claudeai".into()],
        &directory,
        environment,
    )?;
    let input = Arc::new(Mutex::new(process.child.stdin.take()));
    let stdout = process.child.stdout.take().ok_or_else(cli::error)?;
    let stderr = process.child.stderr.take().ok_or_else(cli::error)?;
    let (send, mut receive) = mpsc::channel(2);
    let first = tokio::spawn(read(stdout, send.clone()));
    let second = tokio::spawn(read(stderr, send));
    let (url_send, url_receive) = tokio::sync::oneshot::channel();
    let task = tokio::spawn(async move {
        let mut sender = Some(url_send);
        let outcome=tokio::time::timeout(std::time::Duration::from_secs(600),async{loop{tokio::select!{
status=process.child.wait()=>{let status=status.map_err(|_|cli::error())?;if !status.success(){return Err(cli::error());}if sender.is_some(){let opened_url=std::fs::read_to_string(&opened).ok().filter(|text|url::Url::parse(text).ok().is_some_and(|url|cli::login_url(&url)));let printed=if opened_url.is_none(){tokio::time::timeout(std::time::Duration::from_millis(200),receive.recv()).await.ok().flatten()}else{None};if let Some(url)=opened_url.or(printed){if let Some(sender)=sender.take(){let _=sender.send(url);}}}return cli::identify(&executable,&cli::Home::Isolated(directory.clone()),proxy.clone(),None).await;},
url=receive.recv(),if sender.is_some()=>{if let Some(url)=url{if let Some(sender)=sender.take(){let _=sender.send(url);}}},
_=tokio::time::sleep(std::time::Duration::from_millis(50)),if sender.is_some()=>{if let Ok(bytes)=std::fs::read(&opened){if bytes.len()<=16*1024{if let Ok(text)=std::str::from_utf8(&bytes){if let Ok(url)=url::Url::parse(text){if cli::login_url(&url){if let Some(sender)=sender.take(){let _=sender.send(url.to_string());}}}}}}},
}}}).await.map_err(|_|PluginError::authentication("Claude Code sign-in timed out."))?;
        first.abort();
        second.abort();
        if outcome.is_err() {
            let _ = std::fs::remove_dir_all(&directory);
        }
        outcome
    });
    let pending = Pending {
        task: Some(task),
        input,
    };
    let url=tokio::time::timeout(std::time::Duration::from_secs(30),url_receive).await.map_err(|_|PluginError::authentication("Claude Code returned no sign-in page. Use its installed auth login command to check the installation."))?.map_err(|_|cli::error())?;
    Ok((pending, url))
}
#[cfg(test)]
#[path = "../test/login.test.rs"]
mod tests;
