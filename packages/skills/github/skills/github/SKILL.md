---
name: github
description: Work with GitHub through the `gh` CLI, including issues, pull requests, reviews, Actions runs, releases, repository metadata, API queries, and CLI authentication/setup on persistent environments.
---

# GitHub

Use this skill whenever the task involves GitHub and the `gh` CLI is available or should be made available. Prefer direct `gh` commands over browser-only workflows for operations that have stable CLI support.

## Typical Goals

- Inspect, create, update, label, assign, or close issues.
- Inspect, create, checkout, review, comment on, merge, or monitor pull requests.
- Check GitHub Actions runs, jobs, failed logs, and commit statuses.
- Query repository metadata, branches, tags, releases, artifacts, and security or Dependabot alerts when permissions allow.
- Use `gh api` or GraphQL for fields not exposed by first-class subcommands.
- Install or authenticate `gh` on a persistent VM when the user needs reusable GitHub CLI access.

## Operating Principles

- Use `--repo owner/name` when outside the target repository or when ambiguity is possible.
- Prefer `--json` with `--jq` for machine-readable output instead of parsing tables.
- Prefer managed OAuth over device-flow setup. When GitHub credentials are missing, expired, or insufficient, use `RequestOAuthAuthorization` to connect the user's GitHub account, then pass the saved credential to `Exec.credential_env`.
- Confirm destructive or hard-to-reverse actions before running them unless the user explicitly requested the action.
- Do not claim an issue, PR, workflow, or release was changed until the CLI command succeeds.
- If auth or permissions fail, report the concrete blocker and the command that exposed it.
- Keep generated issue and PR text concise and specific; avoid adding unrelated templates or markdown documents unless asked.

## Managed OAuth

Before running a device-flow login or asking the user for a token, check whether a GitHub OAuth credential already exists:

1. Call `ListOAuthCredentials`.
2. If a `github` credential is available and active, use its alias in `Exec.credential_env`; replace `"default"` below with the actual alias returned by `ListOAuthCredentials`:
   ```json
   [{"env_var":"GH_TOKEN","provider":"github","alias":"default","value":"access_token"}]
   ```
3. If no usable GitHub credential exists, call `RequestOAuthAuthorization` with `provider: "github"`, a short stable `alias` such as `"default"`, and a concise `reason`. Request scopes that match the task, for example `["repo"]` for private repository read/write, `["read:org"]` for organization metadata, and `["workflow"]` only when workflow file changes or Actions operations require it.
4. Share the returned `authorization_url` with the user and wait for them to confirm they completed authorization.
5. Call `CompleteOAuthAuthorization` with the returned `state`. If it returns `completed`, retry the GitHub command with `Exec.credential_env` using the returned alias. If it returns `pending`, ask the user to finish the browser flow. If it returns `failed` or `expired`, start a fresh authorization flow.

Managed OAuth keeps tokens out of the transcript. Do not print the token or try to read it directly; inject it only for the subprocess that needs GitHub access. `gh` honors `GH_TOKEN`, so a full `gh auth login` is usually unnecessary when using this flow.

## Initial Checks

```bash
gh --version
gh auth status -h github.com
git remote -v
```

When the repository is known:

```bash
gh repo view owner/name --json nameWithOwner,defaultBranchRef,viewerPermission
```

## Issues

Search issues:

```bash
gh issue list --repo owner/name --search "login timeout session expired" --json number,title,state,url,labels,updatedAt
```

View an issue:

```bash
gh issue view 123 --repo owner/name --json number,title,body,state,url,labels,assignees,comments
```

Create an issue:

```bash
gh issue create --repo owner/name --title "Investigate login timeout on macOS" --body "..."
```

Update or comment:

```bash
gh issue edit 123 --repo owner/name --add-label bug --add-assignee @me
gh issue comment 123 --repo owner/name --body "..."
```

## Pull Requests

List and view PRs:

```bash
gh pr list --repo owner/name --state open --json number,title,author,headRefName,baseRefName,isDraft,reviewDecision,statusCheckRollup
gh pr view 55 --repo owner/name --json number,title,body,state,url,files,commits,reviews,comments
```

Check out a PR when code inspection is needed:

```bash
gh pr checkout 55 --repo owner/name
```

Create or update a PR:

```bash
gh pr create --repo owner/name --base main --head feature-branch --title "Title" --body "..."
gh pr edit 55 --repo owner/name --title "New title" --body "..."
```

Review, comment, and merge:

```bash
gh pr review 55 --repo owner/name --comment --body "..."
gh pr review 55 --repo owner/name --approve --body "..."
gh pr merge 55 --repo owner/name --squash --delete-branch
```

Use the repository's merge policy and user instructions when choosing merge mode.

## Actions And CI

Check PR checks:

```bash
gh pr checks 55 --repo owner/name
```

List and inspect workflow runs:

```bash
gh run list --repo owner/name --limit 20
gh run view <run-id> --repo owner/name --json status,conclusion,event,headBranch,headSha,jobs
gh run view <run-id> --repo owner/name --log-failed
```

Rerun only when requested or when it is clearly part of the task:

```bash
gh run rerun <run-id> --repo owner/name --failed
```

## Releases, Tags, And Artifacts

```bash
gh release list --repo owner/name
gh release view v1.2.3 --repo owner/name --json tagName,name,body,isDraft,isPrerelease,assets
gh release create v1.2.3 --repo owner/name --title "v1.2.3" --notes "..."
gh run download <run-id> --repo owner/name --dir /tmp/artifacts
```

## Advanced API Queries

Use REST when it is straightforward:

```bash
gh api repos/owner/name/pulls/55 --jq '{title, state, user: .user.login}'
```

Use GraphQL for review threads, project items, or nested data:

```bash
gh api graphql -f query='
query($owner:String!, $repo:String!, $number:Int!) {
  repository(owner:$owner, name:$repo) {
    pullRequest(number:$number) {
      title
      reviewThreads(first:50) {
        nodes { isResolved comments(first:10) { nodes { body author { login } } } }
      }
    }
  }
}' -F owner=owner -F repo=name -F number=55
```

## Authentication And Setup

If `gh` is already authenticated, use it. If setup is required on a persistent VM, prefer the managed OAuth flow above and run `gh` commands with `Exec.credential_env`. Install `gh` using the platform package manager or GitHub's documented package source when needed.

Preferred verification:

```bash
gh auth status -h github.com
gh api user --jq '.login'
```

If normal browser handoff is inconvenient in the local VM, use a device flow:

1. Capture an interactive login for `https://github.com/login` with allowed domain `github.com`.
2. Confirm browser session state exists in the local browser profile if that flow is available.
3. Install `python3`, Playwright for Python, Chromium, `xvfb`, and Chromium runtime libraries on the VM if browser automation is needed.
4. Copy the cookie file to the VM, for example `/tmp/github-cookies.json`.
5. Start a live `gh auth login --hostname github.com --git-protocol https --web --skip-ssh-key` process and keep it running.
6. Use Playwright under `xvfb-run` to open `https://github.com/login/device`, reuse saved cookies, enter the one-time code, and authorize the CLI.
7. Re-run the verification commands.

Important details for the GitHub device page:

- Device codes expire quickly; restart the flow if automation stalls.
- GitHub may split the code across inputs `user-code-0` through `user-code-8`; skip the hidden hyphen input.
- Headful Chromium under `xvfb-run -a` may be more reliable than headless Chromium for the final authorization button.

## Failure Handling

- `gh auth status` fails: authenticate or explain the missing credentials.
- `HTTP 404` from `gh api`: check repository name, token scopes, and whether the authenticated user has access.
- Empty search results: report the query used before creating new GitHub objects.
- CI logs unavailable: inspect run permissions, retention, and whether the job is still running.
- Rate limit or SSO failures: surface the exact `gh` error and do not retry blindly.
