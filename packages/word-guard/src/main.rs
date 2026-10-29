#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) = nora_gateway_middleware::serve::<nora_provider_word_guard::Plugin>().await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
