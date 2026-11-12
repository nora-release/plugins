# ChatGPT

Use the models of your ChatGPT plan (Plus, Pro, Business, Enterprise or Edu) in Nora. You sign in with your ChatGPT account the way Codex does, so requests count against your plan instead of an API key.

## Set up

1. Install the plugin.
2. Open **Settings → AI Providers**, add **ChatGPT**, and choose **Sign in**.
3. Your browser opens ChatGPT's sign-in page. When you finish, it returns to Nora on `localhost:1455`.
4. Pick one of your plan's models in the model picker.

## Good to know

- Each model is sent with the instructions ChatGPT's Codex backend expects for it.
- The account shows its 5-hour and weekly usage windows and its remaining credits.
- Port 1455 must be free while you sign in.

## Details

| | |
| --- | --- |
| Plugin | `nora.chatgpt` |
| Provider `chatgpt` | OpenAI Responses at `https://chatgpt.com/backend-api/codex` |
| Connects to | `chatgpt.com`, `auth.openai.com` |
| Listens on | `localhost:1455` (sign-in only) |
| Runs code | Yes, a small program Nora starts for this provider |
