import { mkdir } from "node:fs/promises"
import { upstreamCoreTree } from "../upstream"

const root = new URL("../..", import.meta.url).pathname
const path = `${root}crates/catalog/src/generated/models-dev-snapshot.json.gz`
const tree = await upstreamCoreTree()
const text = await Bun.file(`${tree}/packages/core/src/models-dev/snapshot.txt`).bytes()

async function unchanged() {
  try {
    const existing = Buffer.from(Bun.gunzipSync(await Bun.file(path).bytes()))
    return existing.length === text.length && Buffer.compare(existing, Buffer.from(text)) === 0
  } catch {
    return false
  }
}

if (await unchanged()) {
  console.log(`catalog: snapshot unchanged (${text.length} bytes)`)
} else {
  await mkdir(`${root}crates/catalog/src/generated`, { recursive: true })
  await Bun.write(path, Bun.gzipSync(text, { level: 9 }))
  console.log(`catalog: snapshot written (${text.length} bytes)`)
}
