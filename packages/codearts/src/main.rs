mod auth;
mod gateway;
mod models;
mod provider;
mod sign;
mod usage;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_provider_plugin_sdk::serve(provider::CodeArts::default()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
