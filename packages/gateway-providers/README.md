# Provider plugins for nora-gateway

Nora plugins (manifest `apiVersion` 3) that add model providers to
nora-gateway, Nora's router. They are installed like any Nora plugin
(`nora plugin install kernel:<id> <folder> --trust`, or the Plugins page)
and their accounts are signed in to in Settings → AI Providers.

The `apiVersion` 2 packages in `../providers` serve Nora Router until it is
retired; the two are kept apart.

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

| `workbuddy` | OpenAI Chat | Has code: WorkBuddy (Tencent) on both sites, `workbuddy` and `workbuddy-ai`; browser sign-in, requests shaped for its chat endpoint, credits, daily check-in (China) |

Those without code:  the router sends their requests itself and reads
their models from the vendor's `/models`. The format is described in
Nora's `docs/provider-plugins.md`. Balances (Moonshot, SiliconFlow) come
once plugins with code answer `provider/usage`.

## Plugins with code

They are a Cargo workspace of their own (`Cargo.toml` here), on the
plugin-kit revision whose protocol nora-gateway speaks, and use its
`nora_plugin_runtime_sdk::provider`. Package one as Nora installs it:

```sh
packages/gateway-providers/package.sh workbuddy .build/workbuddy
nora plugin install kernel:nora.workbuddy .build/workbuddy --trust
```

`workbuddy` keeps the protocol modules of the Nora Router package
(`../providers/workbuddy`) and their tests; `src/compat.rs` gives them the
HTTP client and errors they were written against. Its desktop sign-in and
account discovery are left out: a plugin in the router has its own home,
not the user's.
