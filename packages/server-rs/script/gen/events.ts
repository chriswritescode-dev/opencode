import { mkdir } from "node:fs/promises"
import { dirname } from "node:path"
import { fixtures } from "../events/scenarios"
import { masksFor, runFeed, runFixtures } from "../events/typescript"

const records = await runFixtures(fixtures)
const feed = await runFeed()
const output = `${new URL("../..", import.meta.url).pathname}crates/session/tests/corpus/events.json`
await mkdir(dirname(output), { recursive: true })
await Bun.write(output, JSON.stringify({ masks: masksFor(fixtures), fixtures: records, feed }, null, 2) + "\n")
console.log(`events: ${records.length} fixtures`)
