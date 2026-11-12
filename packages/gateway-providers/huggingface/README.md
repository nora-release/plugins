# Hugging Face

Use the models served through [Hugging Face Inference Providers](https://huggingface.co/docs/inference-providers) with your Hugging Face access token. Requests go to Hugging Face's router, which sends each one to a provider that serves the model.

## Set up

1. Install the plugin.
2. Open **Settings → AI Providers**, add **Hugging Face**, and paste an access token from [huggingface.co/settings/tokens](https://huggingface.co/settings/tokens) with permission to call Inference Providers.
3. Pick one of its models in the model picker.

Nora lists the models from Hugging Face's own model list, so new models show up without an update to this plugin.

## Details

| | |
| --- | --- |
| Plugin | `nora.huggingface` |
| Provider `huggingface` | OpenAI Chat Completions at `https://router.huggingface.co/v1` |
| Connects to | `router.huggingface.co` |
| Runs code | No, Nora sends the requests itself |
