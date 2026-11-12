# Provider plugins for nora-gateway

Nora plugins (manifest `apiVersion` 3) that add model providers to
nora-gateway, Nora's router. They are installed like any Nora plugin
(`nora plugin install kernel:<id> <folder> --trust`, or the Plugins page)
and their accounts are signed in to in Settings → AI Providers.

Nora's Plugins page lists them from `gateway-providers.json` at the
repository's root, which `release.py` writes when it publishes the packages.

| Plugin | API | Notes |
| --- | --- | --- |
| `cerebras` | OpenAI Chat | |
| `huggingface` | OpenAI Chat | Hugging Face's router |
| `vercel-ai-gateway` | OpenAI Chat | |
| `opencode` | Anthropic | OpenCode Zen; the key goes as a bearer token |
| `kimi-coding` | Anthropic | Kimi's coding plan; bearer key, `User-Agent: KimiCLI/1.5` |
| `moonshotai` | OpenAI Chat | Moonshot (international) |
| `moonshotai-cn` | OpenAI Chat | Moonshot (China) |
| `siliconflow-global` | OpenAI Chat | SiliconFlow (international) |
| `chatgpt` | OpenAI Responses | Has code: a ChatGPT plan signed in to as Codex CLI does (browser, port 1455); Responses requests shaped for ChatGPT's Codex backend with each model's own instructions; the 5-hour and weekly windows and credits |
| `copilot` | OpenAI Chat | Has code: a GitHub Copilot plan signed in to with GitHub's device flow, as Copilot's editors do (a code typed at github.com/login/device); each model on the APIs Copilot serves it on (Chat Completions, Responses, Anthropic Messages); Auto, Copilot's pick with its session token, which is how a Free plan is served most models; a model Copilot refuses the account picked by hand is explained and left out of its list from then on; requests sent to the account's Copilot API with its session token, `X-Initiator` and vision headers; premium and chat allowances |
| `grok` | OpenAI Responses | Has code: a Grok plan (SuperGrok, X Premium+) signed in to with xAI's device flow, as Grok Build does (a code typed at accounts.x.ai; no Grok CLI needed); requests sent to Grok Build's backend signed as it signs them, with what it turns away left out; each context window a model lists offered as `<model>@<size>`; the period's credits |
| `kiro` | Anthropic | Has code: browser sign-in (Google, GitHub, AWS Builder ID, IAM Identity Center; the browser comes back to a port on this machine) or an API key; Anthropic Messages turned into Kiro's CodeWhisperer calls and back; credits |
| `workbuddy` | OpenAI Chat | Has code: WorkBuddy (Tencent) on both sites, `workbuddy` and `workbuddy-ai`; browser sign-in, requests shaped for its chat endpoint, credits, daily check-in (China) |

Those without code: the router sends their requests itself and reads
their models from the vendor's `/models`. The format is described in
Nora's `docs/provider-plugins.md`. Balances (Moonshot, SiliconFlow) come
once plugins with code answer `provider/usage`.

## Catalog

`gateway-providers.json` at the repository's root is the catalog Nora's
Plugins page installs these from: each plugin's providers, and the download
and SHA-256 of its package (`<id>-<version>-<target>.zip`, or
`<id>-<version>.zip` for one without code). The packages are assets of a
GitHub release. `release.py` builds them all, writes the catalog and, with
`--publish`, uploads them:

```sh
packages/gateway-providers/release.py --tag gateway-providers-2 --publish
```

Raise a plugin's `version` in its manifest when it changes: Nora offers an
update when the catalog lists a newer one. A manifest's `description` is
the line the Plugins page shows. Each plugin's `README.md` is its page in
Nora: the package carries it, and before the plugin is installed Nora reads
it from the folder the catalog's `repository` points to.

## Plugins with code

They are a Cargo workspace of their own (`Cargo.toml` here), on the
plugin-kit revision whose protocol nora-gateway speaks, and use its
`nora_plugin_runtime_sdk::provider`. Package one as Nora installs it:

```sh
packages/gateway-providers/package.sh workbuddy .build/workbuddy
nora plugin install kernel:nora.workbuddy .build/workbuddy --trust
```

`kiro` and `workbuddy` keep the protocol modules of their Nora Router
packages (`../providers/…`) and their tests; `support` gives them the HTTP
client and errors, with the old codes, they were written against, and the
OAuth callback server a browser sign-in comes back to, and OAuth's device flow (`DeviceFlow`) a sign-in by code goes through. `chatgpt`, `copilot` and `grok` are
written anew on plugin-kit's SDK. Reading
an account from the vendor's own app (Kiro CLI and IDE, WorkBuddy desktop)
and account discovery are left out: a plugin in the router has its own
home, not the user's.
