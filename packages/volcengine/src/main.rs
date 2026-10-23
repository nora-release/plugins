mod provider;
mod signing;
mod usage;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    nora_provider::run(provider::Volcengine).await;
}
