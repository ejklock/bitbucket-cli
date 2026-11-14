---
name: bitbucket
description: Work with Bitbucket Cloud pull requests, comments, pipelines and repositories through the bitbucket CLI. Load when the user asks to list, read, review, create, comment on, approve, merge or decline a Bitbucket pull request, or to inspect or trigger a pipeline.
---

# Bitbucket

The binary is `bitbucket`. It must be on PATH and logged in (`bitbucket auth`).

## Prefer the MCP tools

When the `bitbucket` MCP server is connected, use its tools instead of the shell. Both accept the optional arguments `profile` and `workspace`.

- `bitbucket_read`, operations: pr_list, pr_view, pr_diff, pr_diffstat, pr_commits, pr_statuses, pr_activity, pr_list_comments, pr_view_comment, pr_pipelines, pipeline_list, pipeline_view, repo_list, repo_view.
- `bitbucket_write`, operations: pr_create, pr_edit, pr_comment, pr_approve, pr_unapprove, pr_request_changes, pr_merge, pr_decline, pipeline_trigger, pipeline_stop, pipeline_rerun.

## Otherwise use the CLI

Run the same commands in the shell with `--output json` so the result parses: `bitbucket pr list --output json`. Run `bitbucket --help` and `bitbucket <command> --help` for the exact arguments.

## Conventions

- A repository is `workspace/repo-slug`.
- Pick an account with `--profile <name>` or the BITBUCKET_PROFILE environment variable.
- Read freely. Write operations (merge, decline, comment, approve, create, edit, pipeline trigger or stop) change the team's shared state: ask the user for a go-ahead before each one, unless they just asked for it explicitly.
