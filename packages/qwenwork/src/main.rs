#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_provider_plugin_sdk::serve(nora_provider_qoder::Qoder::new(
        nora_provider_qoder::Site::QwenWork,
    ))
    .await
    {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
