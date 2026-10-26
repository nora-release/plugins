mod auth;
mod catalog;
mod device;
mod endpoint;
mod provider;
mod readings;
mod start;
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    nora_provider_plugin_sdk::serve(provider::Zcode::default()).await
}
#[cfg(test)]
#[path = "../test/manifest.test.rs"]
mod manifest_tests;
