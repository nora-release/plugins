# Linear

Lets Nora's agent work with [Linear](https://linear.app) through the `linear` command-line tool.

## What it can do

- Find issues, and check for existing ones before creating a duplicate.
- Create issues with a title, description, team, labels, project, priority and assignee.
- View, update, comment on, assign, move and close issues.
- Look up teams, labels and projects by name.

## Try asking

- "What's assigned to me in Linear this cycle?"
- "File a bug in the Mobile team about the login crash, high priority."
- "Comment on ENG-142 that the fix is in review."

## Sign in

The agent connects your Linear account with OAuth when it first needs it, and keeps the token out of the conversation. It falls back to a personal API key only when OAuth isn't available.

## Requirements

The [`linear` CLI](https://github.com/schpet/linear-cli) (`@schpet/linear-cli`). The agent installs it when it's missing.
