mod config;
mod credentials;
mod provider;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_provider_plugin_sdk::serve(provider::Vertex::default()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
