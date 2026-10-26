mod alpha_search;
mod plugin;
mod provider;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_provider_plugin_sdk::serve(plugin::OpenAIPlugin::default()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
