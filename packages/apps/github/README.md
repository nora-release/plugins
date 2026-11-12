# GitHub

Brings GitHub into Nora: follow pull requests, reviews and checks in a native panel, and let the agent work with your repositories.

## What it can do

- **A board of what needs you**: every open issue and pull request that involves you, one column per repository, the most recently active first. Filter to issues or pull requests, or search.
- **Details beside the board**: open a card to read the discussion, reviews and changed files, see commits and CI/CD checks, comment, and merge, without leaving the board.
- **Give to Nora**: hand any card to Nora with its discussion, in a new chat.
- **Agent tools**: the agent can list your repositories, issues and pull requests.

## Set up

1. Install the plugin and approve its extension in macOS when asked.
2. Open the **GitHub** panel and choose **Continue with GitHub**.
3. Enter the code Nora shows on GitHub and approve the request. You can also connect with a personal access token instead.

## Permissions

| Permission | Why |
| --- | --- |
| Network: `api.github.com`, `github.com`, `avatars.githubusercontent.com` | Reads and updates your pull requests, issues and repositories |
| Network: `private-user-images.githubusercontent.com`, `user-images.githubusercontent.com`, `camo.githubusercontent.com` | Shows the pictures in issues, pull requests and comments, from where GitHub serves them |
| Credentials | Keeps your GitHub sign-in on this Mac |
| Open links | Opens a pull request or issue in your browser |
| Chat | Sends a pull request or issue to a Nora chat |
