mod embeddings;
mod image;
mod provider;
#[cfg(test)]
#[path = "../test/fixtures.rs"]
mod test_support;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    nora_provider::run(provider::GoogleGemini).await;
}
