import { mkdir } from "node:fs/promises"
import { upstreamWorkspace } from "../upstream"

const root = new URL("../..", import.meta.url).pathname
const areas = ["flags", "normalize", "substitute", "discover", "load"]
const corpusRoot = `${root}target/config-corpus`
const corpusHome = `${corpusRoot}/home`
const source = `${root}script/config-corpus/export.ts`
const tree = await upstreamWorkspace()

for (const area of areas) {
  await Bun.write(`${tree}/packages/core/test/opencode-rs-corpus.ts`, Bun.file(source))
  const result = Bun.spawnSync(["bun", "run", "test/opencode-rs-corpus.ts", area], {
    cwd: `${tree}/packages/core`,
    env: { ...process.env, HOME: corpusHome, USERPROFILE: corpusHome, OPENCODE_CORPUS_ROOT: corpusRoot },
    stdout: "pipe",
    stderr: "inherit",
  })
  if (result.exitCode !== 0) {
    console.error(`corpus: exporting ${area} failed`)
    process.exit(1)
  }
  const output = result.stdout.toString()
  const parsed = JSON.parse(output)
  await mkdir(`${root}crates/config/tests/corpus`, { recursive: true })
  await Bun.write(`${root}crates/config/tests/corpus/${area}.json`, output)
  console.log(`corpus: ${area} ${Object.keys(parsed).length} cases`)
}
