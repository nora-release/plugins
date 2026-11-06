#[tokio::main(flavor = "current_thread")]
async fn main() {
    if let Err(error) =
        nora_provider_plugin_sdk::serve(nora_provider_cline::Cline::for_id("clinepass")).await
    {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
#[cfg(test)]
#[path = "../test/manifest.test.rs"]
mod tests;
