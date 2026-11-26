import { mkdir, rm } from "node:fs/promises"
import { upstreamCoreTree } from "../upstream"

const root = new URL("../..", import.meta.url).pathname
const areas = new Map([["select", { crate: "config", file: "shell-select" }]])
const requested = process.argv[2]
const selected = requested === undefined ? [...areas.keys()] : [requested]

for (const area of selected)
  if (!areas.has(area)) {
    console.error(`corpus: unknown area ${area}`)
    process.exit(1)
  }

const source = `${root}script/shell-corpus/export.ts`
const tree = await upstreamCoreTree()
const corpusRoot = `${root}target/shell-corpus`
const corpusHome = `${corpusRoot}/home`
const temp = `${corpusRoot}/tmp`

await mkdir(corpusHome, { recursive: true })
await mkdir(temp, { recursive: true })
await Bun.write(`${tree}/packages/core/test/opencode-rs-shell-corpus.ts`, await Bun.file(source).text())

for (const area of selected) {
  const target = areas.get(area)!
  const directory = `${corpusRoot}/${area}`
  await rm(directory, { recursive: true, force: true })
  await mkdir(directory, { recursive: true })
  const result = Bun.spawnSync(["bun", "run", "test/opencode-rs-shell-corpus.ts", area], {
    cwd: `${tree}/packages/core`,
    env: {
      ...process.env,
      HOME: corpusHome,
      USERPROFILE: corpusHome,
      OPENCODE_CORPUS_ROOT: directory,
      TMPDIR: temp,
    },
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
    .find((line) => line.startsWith("__SHELL_CORPUS__"))
  if (marker === undefined) {
    console.error(`corpus: exporting ${area} produced no output`)
    process.exit(1)
  }
  let parsed: unknown
  try {
    parsed = JSON.parse(marker.slice("__SHELL_CORPUS__".length))
  } catch {
    console.error(`corpus: exporting ${area} produced invalid JSON`)
    process.exit(1)
  }
  if (!Array.isArray(parsed)) {
    console.error(`corpus: exporting ${area} did not produce a JSON array`)
    process.exit(1)
  }
  await mkdir(`${root}crates/${target.crate}/tests/corpus`, { recursive: true })
  await Bun.write(
    `${root}crates/${target.crate}/tests/corpus/${target.file}.json`,
    `${JSON.stringify(parsed, null, 2)}\n`,
  )
  console.log(`corpus: ${area} ${parsed.length} cases`)
}
