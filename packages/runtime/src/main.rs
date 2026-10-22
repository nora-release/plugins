#[tokio::main(flavor = "current_thread")]
async fn main() {
    let arguments = std::env::args_os().skip(1).collect::<Vec<_>>();
    let result = if arguments.first().is_some_and(|value| value == "--catalog") {
        if arguments.len() == 3 {
            nora_provider::catalog::write_catalog(
                arguments[1].as_ref(),
                arguments[2].as_ref(),
                None,
            )
        } else if arguments.len() == 6 {
            nora_provider::catalog::write_catalog(
                arguments[1].as_ref(),
                arguments[2].as_ref(),
                Some((
                    arguments[3].as_ref(),
                    &arguments[4].to_string_lossy(),
                    &arguments[5].to_string_lossy(),
                )),
            )
        } else {
            Err("Usage: nora-provider --catalog <definitions> <output.json> [archives target download-base-url]".into())
        }
    } else if arguments.first().is_some_and(|value| value == "--prepare") {
        if arguments.len() != 3 {
            Err("Usage: nora-provider --prepare <definitions-directory> <output-directory>".into())
        } else {
            nora_provider::definition::prepare(arguments[1].as_ref(), arguments[2].as_ref())
        }
    } else {
        nora_provider_plugin_sdk::serve(nora_provider::BundledProvider::default())
            .await
            .map_err(|error| error.to_string())
    };
    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
