mod auth;
mod catalog;
mod generate;
mod provider;
mod readings;
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    nora_provider_plugin_sdk::serve(provider::CommandCode::default()).await
}
#[cfg(test)]
#[path = "../test/manifest.test.rs"]
mod manifest_tests;
