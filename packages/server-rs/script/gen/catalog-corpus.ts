import { mkdir } from "node:fs/promises"
import { upstreamCoreTree } from "../upstream"

const root = new URL("../..", import.meta.url).pathname
const areas = ["oauth"]
const corpusRoot = `${root}target/catalog-corpus`
const corpusHome = `${corpusRoot}/home`
const temp = `${corpusRoot}/tmp`
const source = `${root}script/catalog-corpus/export.ts`
const tree = await upstreamCoreTree()

await mkdir(temp, { recursive: true })

const outputs = new Map<string, string>()
const counts = new Map<string, number>()

for (const area of areas) {
  await Bun.write(`${tree}/packages/core/test/opencode-rs-catalog-corpus.ts`, Bun.file(source))
  const result = Bun.spawnSync(["bun", "run", "test/opencode-rs-catalog-corpus.ts", area], {
    cwd: `${tree}/packages/core`,
    env: {
      ...process.env,
      HOME: corpusHome,
      USERPROFILE: corpusHome,
      OPENCODE_CORPUS_ROOT: corpusRoot,
      TMPDIR: temp,
    },
    stdout: "pipe",
    stderr: "inherit",
  })
  if (result.exitCode !== 0) {
    console.error(`corpus: exporting ${area} failed`)
    process.exit(1)
  }
  const output = result.stdout.toString()
  const parsed = JSON.parse(output)
  outputs.set(area, output)
  counts.set(area, Object.keys(parsed).length)
}

await mkdir(`${root}crates/catalog/tests/corpus`, { recursive: true })
for (const [area, output] of outputs) {
  await Bun.write(`${root}crates/catalog/tests/corpus/${area}.json`, output)
  console.log(`corpus: ${area} ${counts.get(area)} cases`)
}
