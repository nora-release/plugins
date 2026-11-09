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

These have no code: the router sends their requests itself and reads
their models from the vendor's `/models`. The format is described in
Nora's `docs/provider-plugins.md`. Balances (Moonshot, SiliconFlow) come
once plugins with code answer `provider/usage`.
