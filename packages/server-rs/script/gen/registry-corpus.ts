import { mkdir } from "node:fs/promises"
import { upstreamCoreTree } from "../upstream"

const root = new URL("../..", import.meta.url).pathname
const source = `${root}script/registry-corpus/export.ts`
const tree = await upstreamCoreTree()

const targets = [
  { directory: "crates/registry/tests/corpus", areas: ["markdown", "scan", "agents", "commands", "instructions", "skills", "references"] },
]

for (const target of targets) {
  for (const area of target.areas) {
    await Bun.write(`${tree}/packages/core/test/opencode-rs-registry-corpus.ts`, Bun.file(source))
    const result = Bun.spawnSync(["bun", "run", "test/opencode-rs-registry-corpus.ts", area], {
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
    await mkdir(`${root}${target.directory}`, { recursive: true })
    await Bun.write(`${root}${target.directory}/${area}.json`, output)
    console.log(`corpus: ${area} ${parsed.length} cases`)
  }
}
