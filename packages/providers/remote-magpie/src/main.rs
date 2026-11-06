mod cache;
mod config;
mod models;
mod pricing;
mod provider;
mod transport;
mod usage;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_provider_plugin_sdk::serve(provider::Remote::default()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
#[path = "../test/support.rs"]
mod test_support;
