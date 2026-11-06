mod cli;
mod cli_messages;
mod cli_output;
mod cli_sessions;
mod cli_usage;
mod learned_usage;
mod login;
mod mcp;
mod plugin;
mod prompt;
mod provider;
mod proxy;
mod search_results;
mod session;
mod session_control;
mod stream;
mod structured_output;
mod supervisor;
mod web_search;

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
