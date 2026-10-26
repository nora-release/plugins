mod browser;
mod cache;
mod cdp;
mod cli;
mod credentials;
mod credits;
mod models;
mod provider;
mod settings;

#[cfg(test)]
#[path = "../test/support.rs"]
mod test_support;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_provider_plugin_sdk::serve(provider::GcpAi::default()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
