---
name: linear
description: Work with Linear through the `linear` CLI, including issue listing, creation, updates, comments, team/project/label lookup, JSON issue views, and CLI authentication/setup on persistent environments.
---

# Linear

Use this skill whenever the task involves Linear and the `linear` CLI is available or should be made available. Prefer CLI workflows for listing, creating, viewing, and updating Linear records when the requested task can be completed without the browser UI.

These notes are grounded against `@schpet/linear-cli`. The latest published version is **2.0.0**; flag names differ from 1.9.1 in the auth subcommand. Always run `linear auth login --help` before attempting non-interactive auth, as flag names vary between versions.

## Typical Goals

- Check existing issues before creating duplicates.
- Create issues with clear titles, descriptions, team assignment, labels, projects, priorities, or assignees when requested.
- View, update, comment on, assign, transition, or close issues.
- List teams, labels, and projects to resolve names, keys, and IDs.
- Configure reusable CLI authentication on a persistent VM.
- Use the Linear web UI only when the CLI lacks a needed operation or setup requires a personal API key.

## Operating Principles

- Inspect `linear --help` and subcommand help when command syntax is uncertain; CLI versions can differ.
- Prefer structured or explicit output flags when supported by the installed CLI. In `1.9.1`, `linear issue view --json` and `linear label list --json` support JSON; `linear issue list` does not.
- Prefer managed OAuth over browser/API-key setup. When Linear credentials are missing, expired, or insufficient, use `RequestOAuthAuthorization` to connect the user's Linear account, then pass the saved credential to `Exec.credential_env`.
- Check existing issues before creating a new one unless the user explicitly asks to create a new issue.
- Do not claim an issue was found, created, or updated until a `linear` command succeeds and returns an identifier or URL.
- If auth or permissions fail, report the concrete blocker and the command that exposed it.
- Avoid relying on browser login alone as proof of CLI access; verify with real CLI reads.

## Managed OAuth

Before doing browser-assisted setup or asking for an API key, check whether a Linear OAuth credential already exists:

1. Call `ListOAuthCredentials`.
2. If a `linear` credential is available and active, use its alias in `Exec.credential_env`; replace `"default"` below with the actual alias returned by `ListOAuthCredentials`:
   ```json
   [{"env_var":"LINEAR_ACCESS_TOKEN","provider":"linear","alias":"default","value":"access_token"}]
   ```
3. If no usable Linear credential exists, call `RequestOAuthAuthorization` with `provider: "linear"`, a short stable `alias` such as `"default"`, and a concise `reason`. Request scopes for the intended work; omit `scopes` for read-only access, or pass `["read", "write"]` when mutations such as creating, updating, commenting, or assigning issues are needed.
4. Share the returned `authorization_url` with the user and wait for them to confirm they completed authorization.
5. Call `CompleteOAuthAuthorization` with the returned `state`. If it returns `completed`, retry the Linear command with `Exec.credential_env` using the returned alias. If it returns `pending`, ask the user to finish the browser flow. If it returns `failed` or `expired`, start a fresh authorization flow.

Managed OAuth keeps tokens out of the transcript. Do not print the token or try to read it directly; inject it only for the subprocess that needs Linear access.

## Initial Checks

```bash
linear --help
linear auth list
linear auth whoami
linear team list
```

If `linear` is not installed, check whether `npx` can run the CLI:

```bash
npx -y @schpet/linear-cli --help
```

## Issue Discovery And Triage

`linear issue list` in version `1.9.1` lists issues but does not expose a full-text search flag. Use filters to narrow the candidate set, then inspect likely matches. If true full-text search is required, use the Linear web UI or API after noting the CLI limitation.

```bash
linear issue list --help
linear issue list --all-states --all-assignees --limit 50 --no-pager
linear issue list --all-states --all-assignees --team CUE --limit 50 --no-pager
linear issue list --state started --state unstarted --assignee self --no-pager
```

Useful `issue list` flags in `1.9.1`:

- `--state <state>` repeatable, with values `triage`, `backlog`, `unstarted`, `started`, `completed`, `canceled`.
- `--all-states`.
- `--assignee <username>`, `--all-assignees`, and `--unassigned`.
- `--team <team>`.
- `--project <project>`.
- `--sort manual|priority`.
- `--limit <limit>`.
- `--no-pager`.

Inspect candidates with JSON when downstream parsing is useful:

```bash
linear issue view <issue-id-or-key>
linear issue view <issue-id-or-key> --json --no-comments --no-download
linear issue url <issue-id-or-key>
linear issue title <issue-id-or-key>
```

When deciding whether an issue is a duplicate, compare:

- User-visible symptom.
- Affected product, platform, or workflow.
- Recency and status.
- Existing owner, team, label, or project context.

## Create Issues

Before creating, resolve the target team and optional metadata:

```bash
linear team list
linear label list --all
linear label list --all --json
linear project list --all-teams
```

Create the issue non-interactively when the requested fields are known. Keep the description specific and actionable:

```bash
linear issue create --team CUE --title "Investigate login timeout on macOS" --description "..." --label Bug --priority 2 --no-interactive
```

Useful `issue create` flags in `1.9.1`:

- `--title <title>`.
- `--description <description>`.
- `--team <team>`.
- `--assignee <assignee>` or `--assignee self`.
- `--label <label>` repeatable.
- `--project <project>`.
- `--state <state>`.
- `--priority <priority>` where `1` is highest and `4` is lowest.
- `--estimate <estimate>`.
- `--due-date <dueDate>`.
- `--parent <team_number>`.
- `--start`.
- `--no-interactive`.

After creation, capture and report:

- Issue key or identifier.
- Title.
- URL.
- Team and status if available.

## Update, Comment, And Transition

Inspect command help before mutating records:

```bash
linear issue update --help
linear issue comment --help
linear issue comment add --help
```

Common operations include:

- Add a comment with investigation notes or a user-provided update.
- Assign or unassign an issue.
- Change status or workflow state.
- Add labels, project, priority, estimate, parent, or due date.
- Close or reopen an issue.

Examples:

```bash
linear issue update CUE-123 --state started --assignee self
linear issue update CUE-123 --label Bug --label Regression --priority 1
linear issue comment add CUE-123 --body "Investigated the timeout path; next step is checking session refresh."
linear issue comment list CUE-123
```

After mutation, re-view the issue or rely on the command's returned identifier/URL to verify the change.

## Projects, Teams, And Workflow Context

Use lookup commands to avoid guessing names or IDs:

```bash
linear team list
linear team members CUE
linear label list --all
linear label list --team CUE
linear label list --workspace
linear project list --all-teams
linear project list --team CUE
linear project view <projectId>
```

The `1.9.1` top-level command set includes `auth`, `issue`, `team`, `project`, `project-update`, `milestone`, `initiative`, `initiative-update`, `label`, `document`, `config`, and `schema`. It does not expose top-level `user` or `cycle` commands.

If workflow-state names are uncertain, inspect issue details and use `linear issue update --state <state>` with a known state name or type.

## Authentication And Setup

If `linear auth whoami` already works, do not recreate credentials. If setup is required, prefer the managed OAuth flow above and run Linear commands with `Exec.credential_env`. Configure `@schpet/linear-cli` with a manually obtained API key only when managed OAuth is unavailable or the CLI cannot use the injected OAuth token.

Success criteria for setup:

- Interactive login state for `linear.app` exists if browser-based API key creation is needed.
- `@schpet/linear-cli` is installed or invokable through a persistent wrapper.
- A Linear API key is available to the CLI.
- `linear auth whoami` succeeds.
- A real read command such as `linear team list` succeeds.

Setup workflow:

1. Check for Node/npm and prior CLI state with `linear auth list`.
2. Install `@schpet/linear-cli` globally with `npm install -g @schpet/linear-cli`.
3. After global install, the binary may land in the nvm version bin path rather than a `PATH` directory. Check with `npm bin -g` or `ls $(npm root -g)/../bin/linear`. **Prefer creating a symlink** into a persistent `PATH` directory over an `npx` wrapper (which re-downloads on every invocation):
   ```bash
   ln -sf "$(npm bin -g)/linear" /home/sprite/.local/bin/linear
   ```
4. Run `linear auth login --help` to confirm the exact flag names for the installed version before attempting non-interactive auth.
5. In local VM environments without a system keyring, always pass `--plaintext` to `linear auth login` so credentials are stored in a file rather than hanging on keyring access:
   ```bash
   linear auth login --key <api-key> --plaintext
   ```
6. Restrict permissions on any file containing the token. Store a backup in `/home/sprite/.config/linear/env.sh` exporting `LINEAR_API_KEY`, chmod 600. Avoid printing `linear auth token` unless the user explicitly needs the token.
7. Verify with `linear auth whoami` and `linear team list`.

## Browser-Assisted API Key Creation

Use this only when no usable API key exists and the user needs CLI access.

**Important: the Linear GraphQL API does not accept session cookies.** A personal API key (`lin_api_...`) is the only programmatic auth method. Playwright is required to create one interactively.

1. Request interactive login at `https://linear.app/login`.
2. Confirm browser session state exists in the local browser profile if that flow is available:
   - `cookies.json`
   - `localstorage.json`
   - `indexeddb.json`
3. Copy all three files to the cloud VM. Restore cookies and localStorage in Playwright before navigating to the API key page. IndexedDB is typically empty in the capture; skip rebuilding it unless the app fails to load.
4. Navigate to `https://linear.app/robots.txt` first to establish the origin, then inject `localStorage`, then navigate to the **personal** API key page:
   ```
   https://linear.app/<workspace-slug>/settings/account/security
   ```
   > **Note:** `/settings/api` is the *workspace-level* member API key admin page — it does not have a personal key creation form. The personal key button ("New API key") is only on the account security page above.
5. Click the **"New API key"** button, fill the label input (`placeholder="A descriptive name for this API key…"`), then click **"Create key"**.
6. Capture the one-time token immediately from the rendered page text or from the `ApiKeyCreate` GraphQL response. The token begins with `lin_api_`.
7. Persist the token for CLI auth and verify with the CLI.

### Playwright on cloud VM

Playwright's bundled Chromium download frequently fails in network-restricted cloud VMs. Use the pre-installed system Chrome instead:

```python
browser = p.chromium.launch(
    executable_path='/usr/bin/google-chrome',
    headless=True,
    args=['--no-sandbox', '--disable-setuid-sandbox', '--disable-dev-shm-usage', '--disable-gpu']
)
```

Check for system Chrome with `which google-chrome chromium-browser chromium` before attempting any Playwright browser install.

Important setup details:

- Cookies alone may not restore a Linear session; use cookies and localStorage together.
- The API key is shown only once — capture it from the network response or page text before closing the browser.
- The observed creation mutation is `ApiKeyCreate`, and the token value begins with `lin_api_`.
- Do not verify setup with commands that can fail for unrelated local config; prefer `linear team list`.

## Failure Handling

- Login page appears after state restore: ensure localStorage is injected after `robots.txt` loads but before navigating to the settings page.
- `linear auth login` hangs or fails silently on Linux: pass `--plaintext` to bypass the system keyring.
- `--api-key` flag not recognised: the flag was renamed to `--key` (`-k`) in v2.x. Run `linear auth login --help` to confirm.
- `linear` command missing after `npm install -g`: the binary is in the nvm version bin path. Symlink it: `ln -sf "$(npm bin -g)/linear" /home/sprite/.local/bin/linear`.
- Workspace temporarily shows setup or load errors: wait, retry, and navigate directly to account security once the workspace slug is known.
- Token is not visible after creation: intercept the `ApiKeyCreate` mutation response via Playwright's `page.on('response', ...)` handler.
- Network calls fail with `Operation not permitted`: the sandbox blocked access to `https://api.linear.app/graphql`; rerun the read with network permission if the task requires live Linear data.
- `whoami` works but issue listing fails: run `linear team list`; treat issue-list errors caused by filters, sort, or project config as non-auth failures.
- Need text search and `linear issue list` cannot narrow enough: state that the CLI lacks full-text search in the observed version and use the Linear web UI/API if available.
- No Linear access: clearly report the access blocker and do not fabricate issue data.
