#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_provider_plugin_sdk::serve(nora_provider_workbuddy::WorkBuddy::new(
        nora_provider_workbuddy::Site::China,
    ))
    .await
    {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
