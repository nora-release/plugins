#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) =
        nora_gateway_middleware::serve::<nora_provider_system_prompt::Plugin>().await
    {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
