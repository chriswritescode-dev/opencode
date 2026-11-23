import { pin } from "../upstream"

const root = new URL("../..", import.meta.url).pathname

const commit = await pin()
const result = Bun.spawnSync(["git", "show", `${commit}:packages/protocol/openapi.json`], { cwd: root })
if (result.exitCode !== 0) {
  console.error(`openapi: git show ${commit}:packages/protocol/openapi.json failed`)
  process.exit(1)
}
await Bun.write(`${root}crates/contract/openapi.json`, result.stdout)
