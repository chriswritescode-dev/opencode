import { mkdir } from "node:fs/promises"
import { upstreamCoreTree } from "../upstream"

const root = new URL("../..", import.meta.url).pathname
const tree = await upstreamCoreTree()
const destination = `${root}crates/registry/src/generated/text`

const files = [
  ["packages/core/src/plugin/command/initialize.txt", "initialize.txt"],
  ["packages/core/src/plugin/command/review.txt", "review.txt"],
  ["packages/core/src/plugin/skill/opencode.md", "opencode.md"],
  ["packages/core/src/plugin/skill/report.md", "report.md"],
]

await mkdir(destination, { recursive: true })

for (const [source, name] of files) {
  const bytes = await Bun.write(`${destination}/${name}`, Bun.file(`${tree}/${source}`))
  console.log(`text: ${name} ${bytes} bytes`)
}
