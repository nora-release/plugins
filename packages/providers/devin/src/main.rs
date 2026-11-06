mod account;
mod auth;
#[cfg(test)]
#[path = "../test/fixture.rs"]
mod fixture;
mod models;
mod network;
mod protobuf;
mod provider;
mod request;
mod schema;
mod stream;
mod usage;
#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_provider_plugin_sdk::serve(provider::Devin::default()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
