# Kimi Coding

Use your [Kimi](https://www.kimi.com) coding plan in Nora with its API key.

## Set up

1. Install the plugin.
2. Open **Settings → AI Providers**, add **Kimi Coding**, and paste the API key of your Kimi coding plan.
3. Pick one of its models in the model picker.

Nora lists the models from Kimi's own model list, so new models show up without an update to this plugin.

## Good to know

Requests use the Anthropic Messages format, with the key sent as a bearer token and the user agent Kimi's coding endpoint expects.

## Details

| | |
| --- | --- |
| Plugin | `nora.kimi-coding` 1.0.0 |
| Provider `kimi-coding` | Anthropic Messages at `https://api.kimi.com/coding` |
| Connects to | `api.kimi.com` |
| Runs code | No, Nora sends the requests itself |
