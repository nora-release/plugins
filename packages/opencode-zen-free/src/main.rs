mod catalog;
mod collapse;
mod protocol;
mod provider;
mod session;
mod sse;
mod tools;
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    nora_provider_plugin_sdk::serve(provider::ZenFree::default()).await
}
#[cfg(test)]
#[path = "../test/manifest.test.rs"]
mod manifest_tests;
