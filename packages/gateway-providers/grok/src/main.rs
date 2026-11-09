#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    nora_gateway_grok::serve().await
}
