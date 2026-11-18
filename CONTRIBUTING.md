# Contributing

Thanks for building for Nora. This guide covers adding skill, provider and app plugins to the official catalog, and changing existing ones.

You do not need to be in this catalog to share a plugin: anyone can install one straight from its own repository with `nora plugins add owner/repo`. Add it here when it is useful to most Nora users and you want it on the Plugins page.

## Before you start

- Read the [plugin docs](https://nora.elonehoo.me/docs/plugins/) and try the [examples](https://nora.elonehoo.me/docs/plugins/examples).
- For a new plugin, open an issue first describing what it does. It saves you work if it overlaps with something planned.

## Add a skill plugin

Skill plugins are only instructions and files; they run no code of their own.

1. Create `packages/skills/<name>/` with:

   ```text
   <name>/
   ├── nora-plugin.json
   ├── README.md
   ├── assets/icon.svg
   └── skills/<name>/SKILL.md
   ```

2. Write `nora-plugin.json`:

   ```json
   {
     "apiVersion": 3,
     "id": "nora.<name>",
     "name": "<Display Name>",
     "version": "1.0.0",
     "description": "One sentence: what it lets the agent do.",
     "icon": "assets/icon.svg",
     "runtime": { "kind": "none" },
     "scope": "session",
     "contributes": { "agent.skills": ["skills/<name>"] }
   }
   ```

3. Write the skill in `skills/<name>/SKILL.md`. Its `description` decides when the agent uses it, so say plainly what it is for.

4. Write `README.md` with three sections, as the existing plugins do: **What it can do**, **Try asking** (three example requests) and **Requirements** (tools or packages the skill uses).

5. Add the plugin to [`.nora-plugin/marketplace.json`](.nora-plugin/marketplace.json):

   ```json
   {
     "name": "nora.<name>",
     "description": "Same sentence as the manifest.",
     "version": "1.0.0",
     "source": "packages/skills/<name>"
   }
   ```

6. Check it and try it in Nora:

   ```sh
   nora plugin validate packages/skills/<name>
   nora --plugin-dir packages/skills/<name>
   ```

## Add a provider plugin

A provider whose API follows OpenAI, Anthropic or Gemini needs only a manifest. Copy [`packages/gateway-providers/cerebras`](packages/gateway-providers/cerebras) and change the `id`, `name`, `api`, `baseURL` and `permissions`. See [provider plugins](https://nora.elonehoo.me/docs/plugins/providers) for every field.

A provider that needs a browser sign-in, request signing or a private API needs code. These are Rust crates in the [`packages/gateway-providers`](packages/gateway-providers) workspace; [`chatgpt`](packages/gateway-providers/chatgpt) and [`copilot`](packages/gateway-providers/copilot) are complete examples. Run `make check` before you open a pull request.

Add a row for your provider to the table in [`packages/gateway-providers/README.md`](packages/gateway-providers/README.md). Maintainers package provider plugins and update `gateway-providers.json` when they publish a release, so do not edit it by hand.

## Add an app plugin

App plugins are native Swift code with their own panel in Nora, like [GitHub](packages/apps/github). They can also bring commands for the agent and skills. Contributions are welcome; open an issue first, since an app plugin is more work to review.

1. Create `packages/apps/<name>/` modeled on [`packages/apps/github`](packages/apps/github):

   | File | What it is |
   | --- | --- |
   | `Package.swift` | A Swift package with a host product and an extension product. |
   | `nora-plugin.json` | The manifest: `id`, `version`, `permissions`, `networkDomains`, `panels`, `commands` and `skills`. |
   | `plugin-release.json` | How the release is assembled: products, principal class and resources. |
   | `Makefile` | `make install`, `make test`, `make build` and `make package`. |
   | `README.md` | The plugin's page in Nora. |

2. Build and test it:

   ```sh
   cd packages/apps/<name>
   make install test package
   ```

3. Open a pull request. List every permission and network domain the plugin needs, and why.

You do not need an Apple Developer account. Releases from this catalog are built by CI, ad-hoc signed with the App Sandbox, and trusted by Nora through their checksum in the reviewed catalog. Once your plugin is merged, a maintainer adds its release workflow (like [`release-github-plugin.yml`](.github/workflows/release-github-plugin.yml)) and publishes it by pushing a `<name>-vX.Y.Z` tag.

To ship an app plugin outside this catalog instead, it must be signed with a Developer ID and notarized by Apple, or Nora will not install it.

## Change an existing plugin

- Bump `version` in `nora-plugin.json`, and in `.nora-plugin/marketplace.json` for skill plugins. Plugins update on users' Macs when their version changes.
- Keep the README in step with what the plugin does.

## What reviewers check

- `nora plugin validate` passes, and CI is green.
- It does one clear job, and the README says what it does and what it runs.
- `permissions` lists every network host it contacts, and nothing more.
- It asks before anything destructive and never reads secrets it does not need.
- It works on a fresh Mac with what the README lists under Requirements.

Plugins run with the user's full permissions, so reviewers read every script and every line of code before merging.

## Commit messages

Use conventional commits with the plugin as the scope, for example `feat(pdfs): read encrypted PDFs` or `fix(copilot): refresh expired sessions`.
