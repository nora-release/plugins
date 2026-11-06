#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_provider_redact::serve().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
