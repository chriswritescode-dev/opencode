import { mkdir } from "node:fs/promises"
import { fileURLToPath } from "node:url"
import { upstreamCoreTree } from "../upstream"

const root = fileURLToPath(new URL("../..", import.meta.url))
const source = `${root}script/tool-corpus/export.ts`
const tree = await upstreamCoreTree()

await Bun.write(`${tree}/packages/core/test/opencode-rs-tool-corpus.ts`, await Bun.file(source).text())

for (const area of ["definitions", "snapshot", "validation", "truncate", "read_filesystem", "read", "write", "edit", "patch_parse", "patch", "glob", "grep", "skill"]) {
  const result = Bun.spawnSync(["bun", "run", "test/opencode-rs-tool-corpus.ts", area], {
    cwd: `${tree}/packages/core`,
    stdout: "pipe",
    stderr: "inherit",
  })
  if (result.exitCode !== 0) {
    console.error(`corpus: exporting ${area} failed`)
    process.exit(1)
  }
  const marker = result.stdout
    .toString()
    .split("\n")
    .find((line) => line.startsWith("__TOOL_CORPUS__"))
  if (marker === undefined) {
    console.error(`corpus: exporting ${area} produced no output`)
    process.exit(1)
  }
  let parsed: unknown
  try {
    parsed = JSON.parse(marker.slice("__TOOL_CORPUS__".length))
  } catch {
    console.error(`corpus: exporting ${area} produced invalid JSON`)
    process.exit(1)
  }
  if (!Array.isArray(parsed)) {
    console.error(`corpus: exporting ${area} did not produce a JSON array`)
    process.exit(1)
  }
  await mkdir(`${root}crates/tool/tests/corpus`, { recursive: true })
  await Bun.write(`${root}crates/tool/tests/corpus/${area}.json`, `${JSON.stringify(parsed, null, 2)}\n`)
  console.log(`corpus: ${area} ${parsed.length} cases`)
}
