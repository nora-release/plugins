mod account;
mod attachments;
mod auth;
#[cfg(test)]
#[path = "../test/fixture.rs"]
mod fixture;
mod metadata;
mod models;
mod network;
mod prompt;
mod provider;
mod request;
mod schema;
mod stream;
mod usage;
#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_provider_plugin_sdk::serve(provider::Factory::default()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
