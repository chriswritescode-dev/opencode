import { mkdir } from "node:fs/promises"
import { fileURLToPath } from "node:url"
import { upstreamCoreTree } from "../upstream"

const root = fileURLToPath(new URL("../..", import.meta.url))
const source = `${root}script/fs-corpus/export.ts`
const tree = await upstreamCoreTree()
const areas = ["paths", "mime", "bom", "diff", "filesystem", "access", "fuzzysort", "search"]

for (const area of areas) {
  await Bun.write(`${tree}/packages/core/test/opencode-rs-fs-corpus.ts`, Bun.file(source))
  const result = Bun.spawnSync(["bun", "run", "test/opencode-rs-fs-corpus.ts", area], {
    cwd: `${tree}/packages/core`,
    stdout: "pipe",
    stderr: "inherit",
  })
  if (result.exitCode !== 0) {
    console.error(`corpus: exporting ${area} failed`)
    process.exit(1)
  }
  const output = result.stdout.toString()
  let parsed: unknown
  try {
    parsed = JSON.parse(output)
  } catch {
    console.error(`corpus: exporting ${area} produced invalid JSON`)
    process.exit(1)
  }
  if (!Array.isArray(parsed)) {
    console.error(`corpus: exporting ${area} did not produce a JSON array`)
    process.exit(1)
  }
  await mkdir(`${root}crates/fs/tests/corpus`, { recursive: true })
  await Bun.write(`${root}crates/fs/tests/corpus/${area}.json`, output)
  console.log(`corpus: ${area} ${parsed.length} cases`)
}
