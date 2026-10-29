#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) =
        nora_gateway_middleware::serve::<nora_provider_param_override::Plugin>().await
    {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
