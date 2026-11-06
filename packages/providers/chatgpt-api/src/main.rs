mod auth;
mod body;
mod http;
mod models;
mod provider;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_provider_plugin_sdk::serve(provider::ChatGPTAPI::default()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
#[path = "../test/support.rs"]
mod test_support;
