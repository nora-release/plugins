# GitHub Copilot

Use the models of your [GitHub Copilot](https://github.com/features/copilot) plan in Nora, signed in with GitHub the way Copilot's editors are.

## Set up

1. Install the plugin.
2. Open **Settings → AI Providers**, add **GitHub Copilot**, and choose **Sign in**.
3. Nora shows a code. Enter it at [github.com/login/device](https://github.com/login/device) and approve the request.
4. Pick a model in the model picker, or **Auto** to let Copilot choose.

## Good to know

- Each model is called on the API Copilot serves it on: Chat Completions, Responses or Anthropic Messages.
- **Auto** uses Copilot's own pick, which is how a Copilot Free plan gets most models.
- A model Copilot refuses for your account is explained once and then left out of the list.
- The account shows its premium request and chat allowances.

## Details

| | |
| --- | --- |
| Plugin | `nora.copilot` 1.0.0 |
| Provider `copilot` | OpenAI Chat Completions at `https://api.githubcopilot.com` |
| Connects to | `github.com`, `api.github.com`, `api.githubcopilot.com`, `*.githubcopilot.com` |
| Runs code | Yes, a small program Nora starts for this provider |
