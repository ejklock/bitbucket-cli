import assert from "node:assert/strict"
import { readFileSync } from "node:fs"
import { test } from "node:test"
import { fileURLToPath } from "node:url"
const mod: Record<string, any> = await import("./index.ts").catch((error: any) => {
  if (error?.code === "ERR_MODULE_NOT_FOUND" && String(error.message).includes("index.ts")) return {}
  throw error
})

const skillsDir = fileURLToPath(new URL("./skills", import.meta.url))
const defaultEntry = { type: "local", command: ["bitbucket", "mcp"] }

const run = async (config: Record<string, any>) => {
  assert.equal(typeof mod.server, "function", "the package exports a server plugin")
  const hooks = await mod.server()
  await hooks.config(config)
  return config
}

test("an empty config gets the default bitbucket server and the skills directory", async () => {
  const config = await run({})
  assert.deepEqual(config.mcp.bitbucket, defaultEntry)
  assert.deepEqual(config.skills.paths, [skillsDir])
})

test("a user bitbucket entry is left unchanged", async () => {
  const user = { type: "local", command: ["/opt/bitbucket", "mcp"], environment: { A: "1" }, enabled: false }
  const config = await run({ mcp: { bitbucket: structuredClone(user) } })
  assert.deepEqual(config.mcp.bitbucket, user)
})

test("another user MCP entry is left unchanged", async () => {
  const other = { type: "local", command: ["x"] }
  const config = await run({ mcp: { other: structuredClone(other) } })
  assert.deepEqual(config.mcp.other, other)
  assert.deepEqual(config.mcp.bitbucket, defaultEntry)
})

test("existing skills paths and keys are kept and the directory is appended", async () => {
  const config = await run({ skills: { paths: ["/a"], urls: ["u"] } })
  assert.deepEqual(config.skills, { paths: ["/a", skillsDir], urls: ["u"] })
})

test("running the hook twice changes nothing the second time", async () => {
  const config = await run({})
  const first = structuredClone(config)
  await run(config)
  assert.deepEqual(config, first)
  assert.deepEqual(config.skills.paths, [skillsDir])
})

test("a string mcp value is neither overwritten nor crashed on, and skills are still added", async () => {
  const config = await run({ mcp: "user-text" })
  assert.equal(config.mcp, "user-text")
  assert.deepEqual(config.skills.paths, [skillsDir])
})

test("a string skills value is neither overwritten nor crashed on, and the server is still added", async () => {
  const config = await run({ skills: "user-text" })
  assert.equal(config.skills, "user-text")
  assert.deepEqual(config.mcp.bitbucket, defaultEntry)
})

test("a string skills.paths value is left as written", async () => {
  const config = await run({ skills: { paths: "user-text" } })
  assert.equal(config.skills.paths, "user-text")
})

test("package.json exposes the skills to Pi and ships only index.ts, skills and the README", () => {
  const pkg = JSON.parse(readFileSync(new URL("./package.json", import.meta.url), "utf8"))
  assert.deepEqual(pkg.pi.skills, ["skills"])
  assert.deepEqual(pkg.files, ["index.ts", "skills", "README.md"])
  assert.equal(pkg.name, "bitbucket-agent")
})
