import { expect, test } from "bun:test"
import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises"
import os from "node:os"
import path from "node:path"

import { installRuntime } from "../src/plugin/runtime"
import { modules } from "../src/plugin/runtime-modules.gen"

installRuntime()

async function writeFiles(root: string, files: Record<string, string>) {
  await Promise.all(
    Object.entries(files).map(async ([file, content]) => {
      const target = path.join(root, file)
      await mkdir(path.dirname(target), { recursive: true })
      await writeFile(target, content)
    }),
  )
}

test("plugins and transitive dependencies share the host's effect and @opencode/plugin instances", async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "opencode-plugin-runtime-"))
  try {
    await writeFiles(directory, {
      "node_modules/effect/package.json":
        '{"name":"effect","version":"0.0.0","type":"module","exports":{".":"./index.js"}}',
      "node_modules/effect/index.js":
        "export const Effect = { source: 'fixture' }; export const Schema = { source: 'fixture' }",
      "node_modules/@opencode/plugin/package.json":
        '{"name":"@opencode/plugin","version":"0.0.0","type":"module","exports":{"./effect/plugin":"./effect/plugin.js"}}',
      "node_modules/@opencode/plugin/effect/plugin.js": "export const define = () => ({ source: 'fixture' })",
      "index.ts":
        'import { Effect, Schema } from "effect"; import { define } from "@opencode/plugin/effect/plugin"; export const captured = { Effect, Schema, define }',
    })

    const captured = (await import(path.join(directory, "index.ts"))).captured
    expect(captured.Effect).toBe(modules.effect!().Effect)
    expect(captured.Schema).toBe(modules.effect!().Schema)
    expect(captured.define).toBe(modules["@opencode/plugin/effect/plugin"]!()["define"])
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
})

test("redirects a nested Effect 3 installation to the host, allows effect/package.json, and fails loudly on unprovided subpaths", async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "opencode-plugin-runtime-"))
  try {
    const pluginDir = path.join(directory, "v3-dep-plugin")
    const v3DepDir = path.join(pluginDir, "node_modules/v3-dep")
    const badDir = path.join(directory, "removed-subpath-plugin")
    await writeFiles(directory, {
      "v3-dep-plugin/node_modules/effect/package.json":
        '{"name":"effect","version":"4.0.0-rc.111","type":"module","exports":{"./package.json":"./package.json"}}',
      "v3-dep-plugin/node_modules/v3-dep/node_modules/effect/package.json":
        '{"name":"effect","version":"3.19.19","type":"module","exports":{".":"./index.js","./Option":"./Option.js","./ReadonlyArray":"./ReadonlyArray.js"}}',
      "v3-dep-plugin/node_modules/v3-dep/node_modules/effect/index.js":
        "export const Effect = { major: 3 }; export const Schema = { major: 3 }",
      "v3-dep-plugin/node_modules/v3-dep/node_modules/effect/Option.js": "export const some = () => ({ major: 3 })",
      "v3-dep-plugin/node_modules/v3-dep/node_modules/effect/ReadonlyArray.js": "export const fromIterable = () => []",
      "v3-dep-plugin/node_modules/v3-dep/package.json":
        '{"name":"v3-dep","type":"module","exports":{".":"./index.js","./v3-only":"./v3-only.js"}}',
      "v3-dep-plugin/node_modules/v3-dep/index.js":
        'import { Effect, Schema } from "effect"; import { some } from "effect/Option"; export const v3DepCaptured = { Effect, Schema, some }',
      "v3-dep-plugin/node_modules/v3-dep/v3-only.js":
        'import { fromIterable } from "effect/ReadonlyArray"; export { fromIterable }',
      "v3-dep-plugin/index.ts":
        'import pkg from "effect/package.json" with { type: "json" }; import { v3DepCaptured } from "v3-dep"; export const effectPkgName = pkg.name; export { v3DepCaptured }; export default { id: "v3-dep-plugin", async setup() {} }',
      "removed-subpath-plugin/node_modules/effect/package.json":
        '{"name":"effect","type":"module","exports":{"./RemovedLegacySubpath":"./RemovedLegacySubpath.js"}}',
      "removed-subpath-plugin/node_modules/effect/RemovedLegacySubpath.js": "export const legacy = true",
      "removed-subpath-plugin/index.ts":
        'import { legacy } from "effect/RemovedLegacySubpath"; export default { id: "removed-subpath", async setup() { void legacy } }',
    })

    const imported = await import(path.join(pluginDir, "index.ts"))
    expect(imported.effectPkgName).toBe("effect")
    expect(imported.v3DepCaptured.Effect).toBe(modules.effect!().Effect)
    expect(imported.v3DepCaptured.Schema).toBe(modules.effect!().Schema)
    expect(imported.v3DepCaptured.some).toBe((modules.effect!().Option as { some: unknown }).some)

    await expect(import(path.join(v3DepDir, "v3-only.js"))).rejects.toThrow("is not provided by OpenCode")
    await expect(import(path.join(badDir, "index.ts"))).rejects.toThrow("is not provided by OpenCode")
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
})

test("routes barrel members to their parent module", () => {
  expect(modules["effect/Effect"]!() as unknown).toBe(modules.effect!().Effect)
  expect(modules["effect/Schema"]).toBeDefined()
  expect(modules["effect/Schema"]!() as unknown).toBe(modules.effect!().Schema)
})

test("the generated specifier list equals upstream discovery at the pin", () => {
  const specifiers = Object.keys(modules)
  expect(specifiers).toHaveLength(484)
  expect(specifiers).toContain("effect")
  expect(specifiers).toContain("effect/Schema")
  expect(specifiers).toContain("@opencode/plugin/effect/plugin")
})
