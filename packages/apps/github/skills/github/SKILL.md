---
name: github
description: Work with GitHub issues, pull requests, reviews, Actions runs, releases, and repository metadata, using the GitHub plugin's tools and the `gh` CLI on this Mac.
---

# GitHub

This skill ships with the GitHub plugin and is available while that plugin is installed and enabled.

## Typical Goals

- Inspect, create, update, label, assign, or close issues.
- Inspect, create, checkout, review, comment on, merge, or monitor pull requests.
- Check GitHub Actions runs, jobs, failed logs, and commit statuses.
- Query repository metadata, branches, tags, releases, artifacts, and security or Dependabot alerts when permissions allow.
- Use `gh api` or GraphQL for fields not exposed by first-class subcommands.

## Choosing A Tool

- For a quick list of the user's repositories, or a repository's issues or pull requests, use the GitHub plugin's tools (List GitHub Repositories, List GitHub Issues, List GitHub Pull Requests). They use the account the user connected in the GitHub panel and need no setup.
- For everything else, such as details, comments, reviews, checks, merges, releases, and API queries, run `gh` on this Mac.
- If a plugin tool reports that GitHub is not connected, ask the user to connect their account in the GitHub panel, or fall back to `gh`.

## Operating Principles

- Use `--repo owner/name` when outside the target repository or when ambiguity is possible.
- Prefer `--json` with `--jq` for machine-readable output instead of parsing tables.
- Confirm destructive or hard-to-reverse actions before running them unless the user explicitly requested the action.
- Do not claim an issue, PR, workflow, or release was changed until the command succeeds.
- If auth or permissions fail, report the concrete blocker and the command that exposed it.
- Keep generated issue and PR text concise and specific; avoid adding unrelated templates or markdown documents unless asked.

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

If `gh` is missing, install it with Homebrew (`brew install gh`) when the user agrees. If it is not signed in, ask the user to run `gh auth login` in their own terminal; it opens a browser, so do not try to drive it yourself. Never ask the user to paste a token into the chat.

Verify:

```bash
gh auth status -h github.com
gh api user --jq '.login'
```

## Failure Handling

- `gh auth status` fails: ask the user to run `gh auth login`, or use the plugin's tools for what they cover.
- `HTTP 404` from `gh api`: check repository name, token scopes, and whether the authenticated user has access.
- Empty search results: report the query used before creating new GitHub objects.
- CI logs unavailable: inspect run permissions, retention, and whether the job is still running.
- Rate limit or SSO failures: surface the exact `gh` error and do not retry blindly.
