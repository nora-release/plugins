mod auth;
mod cookies;
mod net;
mod provider;
mod readings;
#[tokio::main(flavor = "current_thread")]
async fn main() -> std::io::Result<()> {
    nora_provider_plugin_sdk::serve(provider::Mimo::default()).await
}
#[cfg(test)]
#[path = "../test/manifest.test.rs"]
mod manifest_tests;

#[cfg(test)]
#[path = "../test/support.rs"]
mod test_support;
