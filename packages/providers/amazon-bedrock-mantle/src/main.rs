#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) =
        nora_provider_plugin_sdk::serve(nora_provider_amazon_bedrock::Bedrock::mantle()).await
    {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
