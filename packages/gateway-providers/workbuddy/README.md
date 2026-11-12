# WorkBuddy

Use [WorkBuddy](https://www.workbuddy.ai) (Tencent) models in Nora. The plugin adds two providers: **WorkBuddy** for the China site and **WorkBuddy (International)** for the international one.

## Set up

1. Install the plugin.
2. Open **Settings → AI Providers** and add **WorkBuddy** or **WorkBuddy (International)**, whichever site your account is on.
3. Choose **Sign in** and finish signing in in your browser.
4. Pick a model in the model picker.

## Good to know

- The account shows its remaining credits.
- On the China site, the plugin also does the daily check-in for you.

## Details

| | |
| --- | --- |
| Plugin | `nora.workbuddy` |
| Provider `workbuddy` | OpenAI Chat Completions at `https://copilot.tencent.com/v2` |
| Provider `workbuddy-ai` | OpenAI Chat Completions at `https://www.workbuddy.ai/v2` |
| Connects to | `copilot.tencent.com`, `www.workbuddy.ai` |
| Runs code | Yes, a small program Nora starts for this provider |
