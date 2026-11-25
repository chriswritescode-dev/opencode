import { mkdir } from "node:fs/promises"
import { upstreamCoreTree } from "../upstream"

const groups = ["empty", "add", "delete", "count", "fileFromPatchChunk", "splitGitPatch", "chunksByFile"] as const

type Group = (typeof groups)[number]
type Entry = Record<string, unknown> & { name: string }
type Corpus = { [Key in Group]: Array<Entry> }

const fields: Record<Group, Array<string>> = {
  empty: ["name", "file", "patch"],
  add: ["name", "file", "content", "patch"],
  delete: ["name", "file", "content", "patch"],
  count: ["name", "patch", "additions", "deletions"],
  fileFromPatchChunk: ["name", "chunk", "file"],
  splitGitPatch: ["name", "text", "truncated", "chunks"],
  chunksByFile: ["name", "text", "truncated", "fallback", "files"],
}

const stringFields: Record<Group, Array<string>> = {
  empty: ["name", "file", "patch"],
  add: ["name", "file", "content", "patch"],
  delete: ["name", "file", "content", "patch"],
  count: ["name", "patch"],
  fileFromPatchChunk: ["name", "chunk"],
  splitGitPatch: ["name", "text"],
  chunksByFile: ["name", "text"],
}

const root = new URL("../..", import.meta.url).pathname
const tree = await upstreamCoreTree()

await Bun.write(
  `${tree}/packages/core/test/opencode-rs-vcs-corpus.ts`,
  Bun.file(`${root}script/vcs-corpus/scenarios.ts`),
)

const result = Bun.spawnSync(["bun", "run", "test/opencode-rs-vcs-corpus.ts"], {
  cwd: `${tree}/packages/core`,
  stdout: "pipe",
  stderr: "inherit",
})
if (result.exitCode !== 0) fail("exporting the vcs patch corpus failed")

const corpus = validate(JSON.parse(result.stdout.toString()))
await mkdir(`${root}crates/git/tests/corpus`, { recursive: true })
await Bun.write(`${root}crates/git/tests/corpus/patch.json`, `${JSON.stringify(corpus, null, 2)}\n`)
for (const group of groups) console.log(`corpus: ${group} ${corpus[group].length} cases`)

function validate(value: unknown): Corpus {
  if (typeof value !== "object" || value === null || Array.isArray(value)) fail("output is not an object")
  const record = value as Record<string, unknown>
  const keys = Object.keys(record)
  if (keys.length !== groups.length || !groups.every((group) => keys.includes(group)))
    fail(`output groups are ${keys.join(", ")}; expected ${groups.join(", ")}`)

  const names = new Set<string>()
  for (const group of groups) {
    const cases = record[group]
    if (!Array.isArray(cases)) fail(`${group} is not an array`)
    for (const item of cases) {
      if (typeof item !== "object" || item === null || Array.isArray(item)) fail(`${group} contains a non-object case`)
      const entry = item as Record<string, unknown>
      const name = entry.name
      if (typeof name !== "string" || name.length === 0) fail(`${group} contains a case without a name`)
      if (names.has(name)) fail(`duplicate case name ${name}`)
      names.add(name)
      for (const field of fields[group])
        if (!(field in entry)) fail(`${group} case ${name} is missing ${field}`)
      const validated = { ...entry, name }
      requireStrings(group, validated)
      requireCase(group, validated)
    }
  }
  return record as unknown as Corpus
}

function requireStrings(group: Group, entry: Entry) {
  for (const field of stringFields[group])
    if (typeof entry[field] !== "string") fail(`${group} case ${entry.name} has a non-string ${field}`)
}

function requireCase(group: Group, entry: Entry) {
  if (group === "empty" || group === "add" || group === "delete") return
  if (group === "count") {
    if (typeof entry.additions !== "number" || typeof entry.deletions !== "number")
      fail(`count case ${entry.name} has non-numeric additions or deletions`)
    return
  }
  if (group === "fileFromPatchChunk") {
    if (entry.file !== null && typeof entry.file !== "string")
      fail(`fileFromPatchChunk case ${entry.name} has a non-string file`)
    return
  }
  if (group === "splitGitPatch") {
    if (!Array.isArray(entry.chunks) || entry.chunks.some((chunk) => typeof chunk !== "string"))
      fail(`splitGitPatch case ${entry.name} has a non-string chunk list`)
    if (typeof entry.truncated !== "boolean") fail(`splitGitPatch case ${entry.name} has a non-boolean truncated`)
    return
  }
  if (!Array.isArray(entry.fallback)) fail(`chunksByFile case ${entry.name} has no fallback array`)
  if (typeof entry.truncated !== "boolean") fail(`chunksByFile case ${entry.name} has a non-boolean truncated`)
  if (entry.files === null || Array.isArray(entry.files) || typeof entry.files !== "object")
    fail(`chunksByFile case ${entry.name} has no files object`)
}

function fail(message: string): never {
  console.error(`vcs-corpus: ${message}`)
  process.exit(1)
}
