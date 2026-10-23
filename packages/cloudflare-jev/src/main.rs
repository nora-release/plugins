use nora_provider_typesafe::{DecisionFamily, DecisionProvider};

#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    nora_provider_plugin_sdk::serve(DecisionProvider::new(DecisionFamily::Cloudflare)).await
}

#[cfg(test)]
#[path = "../test/provider.test.rs"]
mod tests;
