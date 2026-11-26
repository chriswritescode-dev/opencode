import { mkdir } from "node:fs/promises"
import { fileURLToPath } from "node:url"
import { upstreamCoreTree } from "../upstream"

const root = fileURLToPath(new URL("../..", import.meta.url))
const source = `${root}script/tool-definitions/export.ts`
const tree = await upstreamCoreTree()

await Bun.write(`${tree}/packages/core/test/opencode-rs-tool-definitions.ts`, Bun.file(source))

const off = await exportMode("-full_codemode")

const output = {
  off,
  on: await exportMode("full_codemode"),
}

await mkdir(`${root}crates/tool/src/generated`, { recursive: true })
await Bun.write(`${root}crates/tool/src/generated/definitions.json`, `${JSON.stringify(output, null, 2)}\n`)
console.log(`tool-definitions: ${off.length} tools`)

async function exportMode(override: string) {
  const result = Bun.spawnSync(["bun", "run", "test/opencode-rs-tool-definitions.ts", override], {
    cwd: `${tree}/packages/core`,
    stdout: "pipe",
    stderr: "pipe",
  })
  if (result.exitCode !== 0) {
    console.error(`tool-definitions: exporting ${override} failed`)
    console.error(result.stderr.toString())
    process.exit(1)
  }
  const marker = result.stdout
    .toString()
    .split("\n")
    .find((line) => line.startsWith("__TOOL_DEFINITIONS__"))
  if (marker === undefined) {
    console.error(`tool-definitions: exporting ${override} produced no output`)
    process.exit(1)
  }
  let parsed: unknown
  try {
    parsed = JSON.parse(marker.slice("__TOOL_DEFINITIONS__".length))
  } catch {
    console.error(`tool-definitions: exporting ${override} produced invalid JSON`)
    process.exit(1)
  }
  if (!Array.isArray(parsed)) {
    console.error(`tool-definitions: exporting ${override} did not produce an array`)
    process.exit(1)
  }
  return parsed
}
