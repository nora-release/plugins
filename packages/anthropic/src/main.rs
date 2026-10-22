mod cli;
mod cli_usage;
mod login;
mod mcp;
mod plugin;
mod prompt;
mod provider;
mod proxy;
mod session;
mod stream;
mod supervisor;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let args = std::env::args().skip(1).collect::<Vec<_>>();
    if args.first().map(String::as_str) == Some("claude-supervisor") {
        std::process::exit(supervisor::run(&args[1..]));
    }
    if cli::opened_url(&args) {
        return;
    }
    if args.first().map(String::as_str) == Some("claude-mcp-helper") {
        if args.len() != 2 || mcp::helper(std::path::Path::new(&args[1])).await.is_err() {
            std::process::exit(1);
        }
        return;
    }
    if let Err(error) = nora_provider_plugin_sdk::serve(plugin::Claude::default()).await {
        eprintln!("{error}");
        std::process::exit(1);
    }
}

#[cfg(test)]
#[path = "../test/fixture.rs"]
mod fixture;
