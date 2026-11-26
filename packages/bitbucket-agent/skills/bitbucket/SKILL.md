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
- `pipeline_trigger` and `pipeline_rerun` take `wait` to block until the pipeline finishes; `pipeline_view` takes `wait` to follow a pipeline already running (a deploy in progress) until it settles.
- Arguments per operation (`*` required; `repo` is `workspace/repo-slug`; `limit` is results per page, max 100):
  - `pr_list`: repo*, state (list: open, merged, declined, superseded), query, sort, page, limit
  - `pr_view`, `pr_diff`, `pr_statuses`, `pr_approve`, `pr_unapprove`, `pr_request_changes`, `pr_decline`: repo*, id*
  - `pr_diffstat`, `pr_commits`, `pr_activity`, `pr_list_comments`: repo*, id*, limit
  - `pr_view_comment`: repo*, id*, comment_id*
  - `pr_pipelines`: repo*, id*, scan_limit
  - `pipeline_list`: repo*, limit, status, target_branch, sort, page
  - `pipeline_view`: repo*, build*, logs, full_logs, step, wait
  - `repo_list`: limit, role, query, sort, page (`workspace` selects the workspace)
  - `repo_view`: repo*
  - `pr_create`: repo*, title*, source*, destination, body, reviewers (comma-separated), close_source_branch
  - `pr_edit`: repo*, id*, title, body, reviewers, destination
  - `pr_comment`: repo*, id*, body*, path, line (needs path), parent (reply to a comment id)
  - `pr_merge`: repo*, id*, strategy (merge-commit, squash or fast-forward), message, close_source_branch
  - `pipeline_trigger`: repo*, branch, commit, pipeline, var and secured_var (lists of KEY=VALUE), wait
  - `pipeline_stop`: repo*, build or uuid
  - `pipeline_rerun`: repo*, build*, wait

## Otherwise use the CLI

Run the same commands in the shell with `--output json` so the result parses: `bitbucket pr list --output json`. Run `bitbucket --help` and `bitbucket <command> --help` for the exact arguments.

## Conventions

- A repository is `workspace/repo-slug`.
- Pick an account with `--profile <name>` or the BITBUCKET_PROFILE environment variable.
- Read freely. Write operations (merge, decline, comment, approve, create, edit, pipeline trigger or stop) change the team's shared state: ask the user for a go-ahead before each one, unless they just asked for it explicitly.
