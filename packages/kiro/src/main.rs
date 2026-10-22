mod auth;
mod network;
mod provider;
mod request;
mod session;
mod stream;
mod usage;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_provider_plugin_sdk::serve(provider::Kiro::default()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
