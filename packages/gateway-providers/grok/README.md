# Grok

Use the models of your SuperGrok or X Premium+ plan in Nora, signed in with your xAI account the way Grok Build is. You don't need the Grok CLI.

## Set up

1. Install the plugin.
2. Open **Settings → AI Providers**, add **Grok**, and choose **Sign in**.
3. Nora shows a code. Enter it at [accounts.x.ai](https://accounts.x.ai) and approve the request.
4. Pick a model in the model picker.

## Good to know

- When a model offers more than one context window, each one is listed as `<model>@<size>`.
- The account shows the credits left in the current period.

## Details

| | |
| --- | --- |
| Plugin | `nora.grok` 1.1.0 |
| Provider `grok` | OpenAI Responses at `https://cli-chat-proxy.grok.com/v1` |
| Connects to | `auth.x.ai`, `accounts.x.ai`, `cli-chat-proxy.grok.com`, `x.ai`, `storage.googleapis.com` |
| Runs code | Yes, a small program Nora starts for this provider |
