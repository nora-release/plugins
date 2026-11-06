mod auth;
mod provider;
mod readings;
mod stream;
#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_provider_plugin_sdk::serve(provider::Raccoon::default()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
#[cfg(test)]
#[path = "../test/manifest.test.rs"]
mod tests;
