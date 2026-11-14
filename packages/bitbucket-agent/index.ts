import { fileURLToPath } from "node:url"

type McpEntry = { type: "local"; command: string[] } & Record<string, unknown>
type OpencodeConfig = {
  mcp?: unknown
  skills?: unknown
}

const skillsDir = fileURLToPath(new URL("./skills", import.meta.url))

const isRecord = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value)

const defaultEntry: McpEntry = { type: "local", command: ["bitbucket", "mcp"] }

// OpenCode can register an MCP server and a skills directory only by mutating
// the merged config in the `config` hook. A user's own `bitbucket` entry wins
// over the default, and the skills directory is listed once however often the
// hook runs. A value of the wrong type is the user's to fix: it is left as
// written rather than overwritten.
export const server = async () => ({
  config: async (config: OpencodeConfig): Promise<void> => {
    if (config.mcp === undefined) config.mcp = {}
    if (isRecord(config.mcp) && !("bitbucket" in config.mcp)) config.mcp.bitbucket = { ...defaultEntry }

    if (config.skills === undefined) config.skills = {}
    if (!isRecord(config.skills)) return
    if (config.skills.paths === undefined) config.skills.paths = []
    if (Array.isArray(config.skills.paths) && !config.skills.paths.includes(skillsDir)) config.skills.paths.push(skillsDir)
  },
})
