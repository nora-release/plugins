# Contributing

This repository holds Nora's own plugins. Fixes and improvements to them are welcome.

## Writing your own plugin?

Publish it in your own repository. Community plugins are not added to this catalog; people install them straight from your repository or from a marketplace you run:

```sh
nora plugins add owner/repo
nora plugins marketplace add owner/your-marketplace
```

The [plugin docs](https://nora.elonehoo.me/docs/plugins/) explain how to build one, with [examples](https://nora.elonehoo.me/docs/plugins/examples) to start from, and [publishing](https://nora.elonehoo.me/docs/plugins/publishing) shows how to share it.

## Improve an official plugin

1. Open an issue for anything bigger than a small fix, so we can agree on the change first.
2. Make the change and keep the plugin's `README.md` in step with what it does.
3. Check it:

   | Plugin kind | Check |
   | --- | --- |
   | Skill plugins (`packages/skills`) | `nora plugin validate packages/skills/<name>` |
   | Provider plugins (`packages/gateway-providers`) | `make check` |
   | App plugins (`packages/apps`) | `make install test` in the plugin's folder |

4. Try it in Nora: `nora --plugin-dir packages/skills/<name>` for skill plugins.
5. Open a pull request. Maintainers bump versions and publish releases.

## What reviewers check

- CI is green.
- The change does what it says, and nothing else.
- `permissions` and network domains list every host the plugin contacts, and nothing more.
- Nothing destructive happens without asking, and no secrets are read that are not needed.

Plugins run with the user's full permissions, so reviewers read every script and every line of code before merging.

## Commit messages

Use conventional commits with the plugin as the scope, for example `fix(pdfs): keep table borders when rendering` or `fix(copilot): refresh expired sessions`.
