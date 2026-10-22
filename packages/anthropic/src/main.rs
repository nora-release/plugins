mod provider;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    nora_provider::run(provider::Anthropic).await;
}
