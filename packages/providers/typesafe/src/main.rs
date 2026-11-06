#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) =
        nora_provider_plugin_sdk::serve(nora_provider_typesafe::Typesafe::default()).await
    {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
