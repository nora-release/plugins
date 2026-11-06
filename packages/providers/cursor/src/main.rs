mod accounting;
mod auth;
mod models;
mod network;
mod pool;
mod protobuf;
mod provider;
mod request;
mod session;
mod stream;
mod team;
mod transport;
mod usage;

#[cfg(test)]
#[path = "../test/fixture.rs"]
mod fixture;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_provider_plugin_sdk::serve(provider::Cursor::default()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
