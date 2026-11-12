# Cerebras

Run Cerebras models in Nora with a Cerebras Cloud API key. Cerebras serves open models on its own hardware and is known for very fast output.

## Set up

1. Install the plugin.
2. Open **Settings → AI Providers**, add **Cerebras**, and paste an API key from [cloud.cerebras.ai](https://cloud.cerebras.ai).
3. Pick one of its models in the model picker.

Nora lists the models from Cerebras's own model list, so new models show up without an update to this plugin.

## Details

| | |
| --- | --- |
| Plugin | `nora.cerebras` 1.0.0 |
| Provider `cerebras` | OpenAI Chat Completions at `https://api.cerebras.ai/v1` |
| Connects to | `api.cerebras.ai` |
| Runs code | No, Nora sends the requests itself |
