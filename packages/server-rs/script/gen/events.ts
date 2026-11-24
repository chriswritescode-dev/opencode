import { mkdir } from "node:fs/promises"
import { dirname } from "node:path"
import { fixtures } from "../events/scenarios"
import { masks, runFailure, runFeed, runFixtures } from "../events/typescript"

const records = await runFixtures(fixtures)
const feed = await runFeed()
const failure = await runFailure()
const output = `${new URL("../..", import.meta.url).pathname}crates/session/tests/corpus/events.json`
await mkdir(dirname(output), { recursive: true })
await Bun.write(output, JSON.stringify({ masks, fixtures: records, feed, failure }, null, 2) + "\n")
console.log(`events: ${records.length} fixtures`)
