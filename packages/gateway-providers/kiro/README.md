# Kiro

Use [Kiro](https://kiro.dev)'s models in Nora. Sign in with Google, GitHub, AWS Builder ID or IAM Identity Center, or use a Kiro API key.

## Set up

1. Install the plugin.
2. Open **Settings → AI Providers** and add **Kiro**.
3. Choose **Sign in** to sign in in your browser, which then returns to Nora on a local port, or paste a Kiro API key.
4. Pick a model in the model picker.

## Good to know

- Nora sends requests in the Anthropic Messages format, and the plugin turns them into Kiro's calls and the replies back.
- The account shows its remaining credits.

## Details

| | |
| --- | --- |
| Plugin | `nora.kiro` 1.0.0 |
| Provider `kiro` | Anthropic Messages at `https://runtime.us-east-1.kiro.dev` |
| Connects to | `app.kiro.dev`, `prod.us-east-1.auth.desktop.kiro.dev`, `*.kiro.dev`, `*.amazonaws.com` |
| Listens on | `localhost` (sign-in only) |
| Runs code | Yes, a small program Nora starts for this provider |
