# OpenCode

Use the models of [OpenCode Zen](https://opencode.ai/zen), OpenCode's curated set of coding models, with your OpenCode API key.

## Set up

1. Install the plugin.
2. Open **Settings → AI Providers**, add **OpenCode**, and paste an API key from [opencode.ai](https://opencode.ai).
3. Pick one of its models in the model picker.

Nora lists the models from OpenCode Zen's own model list, so new models show up without an update to this plugin.

## Good to know

Requests use the Anthropic Messages format, with the key sent as a bearer token.

## Details

| | |
| --- | --- |
| Plugin | `nora.opencode` |
| Provider `opencode` | Anthropic Messages at `https://opencode.ai/zen` |
| Connects to | `opencode.ai` |
| Runs code | No, Nora sends the requests itself |
