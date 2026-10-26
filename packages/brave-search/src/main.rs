#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_provider_plugin_sdk::serve(
        nora_provider_search_api::SearchApiPlugin::new(nora_provider_search_api::Vendor::Brave),
    )
    .await
    {
        eprintln!("Search provider stopped: {error}");
    }
}
