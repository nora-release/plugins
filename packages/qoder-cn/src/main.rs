#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_provider_plugin_sdk::serve(nora_provider_qoder::Qoder::new(
        nora_provider_qoder::Site::China,
    ))
    .await
    {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
#[cfg(test)]
#[path = "../test/manifest.test.rs"]
mod tests;
