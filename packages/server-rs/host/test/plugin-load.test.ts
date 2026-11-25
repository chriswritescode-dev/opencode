import { expect, test } from "bun:test"
import { mkdir, mkdtemp, readFile, rm, writeFile } from "node:fs/promises"
import os from "node:os"
import path from "node:path"

import { installRuntime } from "../src/plugin/runtime"
import { activate, deactivate } from "../src/plugin/activate"
import { load, type LoadParams } from "../src/plugin/load"
import type { NpmPaths } from "../src/plugin/npm"
import type { Context } from "../src/connection"

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

function npmPaths(directory: string): NpmPaths {
  return {
    cache: directory,
    state: directory,
    data: directory,
    config: directory,
    tmp: directory,
    bin: directory,
    log: directory,
    repos: directory,
    home: directory,
  }
}

function loadParams(directory: string): LoadParams {
  return {
    operation: { type: "add", target: directory, options: {} },
    install: false,
    paths: npmPaths(directory),
  }
}

function connection(directory: string): Context {
  return {
    id: 1,
    location: { directory },
    signal: new AbortController().signal,
    call: async (method) => {
      if (method === "plugin.watch") return { value: null }
      if (method === "plugin.list") return { value: { location: { directory }, data: [{ id: "listed" }] } }
      throw new Error("unexpected " + method)
    },
    stream: async function* () {},
  }
}

const app = { name: "opencode", version: "0.0.0", channel: "local" }

test("rejects with a load error when no entrypoint exists", async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "opencode-plugin-load-"))
  try {
    const error = await load(loadParams(directory), connection(directory)).then(
      () => undefined,
      (cause: unknown) => cause,
    )
    expect(error).toBeInstanceOf(Error)
    expect((error as Error).name).toBe("PluginModule.LoadError")
    expect((error as Error).message).toBe(`Plugin entrypoint not found: ${directory}`)
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
})

test("rejects a default export without a definition", async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "opencode-plugin-load-"))
  try {
    await writeFiles(directory, { "index.ts": "export default {}" })
    const error = await load(loadParams(directory), connection(directory)).then(
      () => undefined,
      (cause: unknown) => cause,
    )
    expect(error).toBeInstanceOf(Error)
    expect((error as Error).name).toBe("PluginModule.LoadError")
    expect((error as Error).message).toBe(
      "Plugin must export a default definition with an id and an effect or setup function.",
    )
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
})

test("ignores a deactivate for another location", async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "opencode-plugin-load-"))
  const first = path.join(directory, "first")
  const second = path.join(directory, "second")
  const closed = path.join(first, "closed.txt")
  try {
    await writeFiles(first, {
      "index.ts": `
        import { Effect } from "effect"
        import { appendFileSync } from "node:fs"
        export default {
          id: "closing",
          effect: () =>
            Effect.gen(function* () {
              yield* Effect.addFinalizer(() => Effect.sync(() => appendFileSync(${JSON.stringify(closed)}, "closed")))
            }),
        }
      `,
    })

    const firstConnection = connection(first)
    const firstLoad = await load(loadParams(first), firstConnection)
    if ("pending" in firstLoad) throw new Error("unexpected pending load")
    const list = async () => (await firstConnection.call("plugin.list", null)).value
    const result = await activate(
      { handle: firstLoad.handle, options: {}, location: { directory: first }, app },
      list,
      { directory: first },
    )

    await deactivate(result.activation, { directory: second })
    expect(await readFile(closed, "utf8").catch(() => undefined)).toBeUndefined()

    await deactivate(result.activation, { directory: first })
    expect(await readFile(closed, "utf8")).toBe("closed")
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
})

test("activates a plugin with the listed context and rejects unavailable domains", async () => {
  const directory = await mkdtemp(path.join(os.tmpdir(), "opencode-plugin-load-"))
  const first = path.join(directory, "first")
  const second = path.join(directory, "second")
  const marker = path.join(first, "listed.json")
  try {
    await writeFiles(first, {
      "index.ts": `
        import { Effect } from "effect"
        import { writeFile } from "node:fs/promises"
        export default {
          id: "listed",
          effect: (ctx) =>
            Effect.gen(function* () {
              const result = yield* ctx.plugin.list()
              yield* Effect.promise(() => writeFile(${JSON.stringify(marker)}, JSON.stringify(result)))
            }),
        }
      `,
    })
    await writeFiles(second, {
      "index.ts": `
        import { Effect } from "effect"
        export default {
          id: "blocked",
          effect: (ctx) =>
            Effect.gen(function* () {
              yield* ctx.command.list()
            }),
        }
      `,
    })

    const firstConnection = connection(first)
    const firstLoad = await load(loadParams(first), firstConnection)
    if ("pending" in firstLoad) throw new Error("unexpected pending load")
    const list = async () => (await firstConnection.call("plugin.list", null)).value
    await activate({ handle: firstLoad.handle, options: {}, location: { directory: first }, app }, list, { directory: first })
    expect(JSON.parse(await readFile(marker, "utf8"))).toEqual({
      location: { directory: first },
      data: [{ id: "listed" }],
    })

    const secondLoad = await load(loadParams(second), connection(second))
    if ("pending" in secondLoad) throw new Error("unexpected pending load")
    const error = await activate(
      { handle: secondLoad.handle, options: {}, location: { directory: second }, app },
      list,
      { directory: second },
    ).then(
      () => undefined,
      (cause: unknown) => cause,
    )
    expect(error).toBeInstanceOf(Error)
    expect((error as Error).name).toBe("PluginActivationFailed")
    expect((error as Error).message).toContain("command.list is not available in the sidecar yet (OR-63)")
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
})
