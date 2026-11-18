# Nora plugins

The official plugins for [Nora](https://nora.elonehoo.me), and the catalogs Nora's Plugins page reads them from.

Want to write your own plugin? Start with the [plugin docs](https://nora.elonehoo.me/docs/plugins/), then see [CONTRIBUTING.md](CONTRIBUTING.md) to add one here.

## What's here

| Folder | Plugins | Listed in |
| --- | --- | --- |
| [`packages/skills`](packages/skills) | Skill plugins: [docs](packages/skills/docs), [format-converter](packages/skills/format-converter), [linear](packages/skills/linear), [pdfs](packages/skills/pdfs), [spreadsheets](packages/skills/spreadsheets), [ui-designer](packages/skills/ui-designer) | [`.nora-plugin/marketplace.json`](.nora-plugin/marketplace.json) |
| [`packages/gateway-providers`](packages/gateway-providers) | Model providers for Nora's gateway: ChatGPT, GitHub Copilot, Grok, Kiro, Kimi and more | [`gateway-providers.json`](gateway-providers.json) |
| [`packages/apps`](packages/apps) | Native app plugins with their own panels: [GitHub](packages/apps/github) | [`registry.json`](registry.json) → [`marketplace.json`](marketplace.json) |
| [`packages/environments`](packages/environments) | Experimental; not listed yet. | |

## Install

Open the **Plugins** page in Nora and install from there. Official plugins update themselves while **Update plugins automatically** is on in Settings › About.

From the terminal:

```sh
nora plugins marketplace add nora-release/plugins
nora plugins add nora.pdfs
```

Provider plugins add accounts to **Settings › AI Providers** once installed.

## A plugin's folder

Every plugin has a `nora-plugin.json` manifest and a `README.md`, which Nora shows as the plugin's page. A skill plugin looks like this:

```text
packages/skills/pdfs/
├── nora-plugin.json
├── README.md
├── assets/icon.svg
└── skills/pdfs/SKILL.md
```

## Develop

| Command | What it does |
| --- | --- |
| `nora plugin validate packages/skills/pdfs` | Checks one plugin folder. |
| `make check` | Formats, lints and tests the provider plugins written in Rust. |
| `make marketplace-check` | Validates the app plugin catalog and its release archives. |

CI runs `make check` and `make marketplace-check` on every pull request.

## Licenses

Third-party code that plugins build on is listed with its license in [`LICENSES`](LICENSES).
