mod account;
mod auth;
#[cfg(test)]
#[path = "../test/fixture.rs"]
mod fixture;
mod models;
mod network;
mod provider;
mod request;
mod stream;
mod whole;
#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_provider_plugin_sdk::serve(provider::Zed::default()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
