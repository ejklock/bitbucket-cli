# bitbucket-agent

Bitbucket for AI coding agents. It runs `bitbucket mcp` from PATH, so install the `bitbucket` CLI first (https://github.com/ejklock/bitbucket-cli).

- OpenCode: gives the `bitbucket` MCP server and the `bitbucket` skill. Add `"plugin": ["bitbucket-agent"]` to `opencode.json`. An existing `mcp.bitbucket` entry of yours is kept as written.
- Pi: gives the `bitbucket` skill. Run `pi install npm:bitbucket-agent`.

The skill tells the agent to prefer the MCP tools `bitbucket_read` and `bitbucket_write`, and otherwise to run the CLI with `--output json`.

MIT license.
