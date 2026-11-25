import { test, expect } from "bun:test"
import { mkdtemp, readdir, rm, stat } from "node:fs/promises"
import os from "node:os"
import path from "node:path"
import { pathToFileURL } from "node:url"
import { npm } from "../src/plugin/npm"
import { startRegistry } from "./fixtures/registry"

async function workspace() {
  const root = await mkdtemp(path.join(os.tmpdir(), "plugin-npm-"))
  const paths = {
    cache: path.join(root, "cache"),
    state: path.join(root, "state"),
    data: path.join(root, "data"),
    config: path.join(root, "config"),
    tmp: path.join(root, "tmp"),
    bin: path.join(root, "bin"),
    log: path.join(root, "log"),
    repos: path.join(root, "repos"),
    home: root,
  }
  return {
    root,
    paths,
    cleanup: () => rm(root, { recursive: true, force: true }),
  }
}

function useNpmEnvironment(registry: string, root: string) {
  const previousRegistry = process.env.npm_config_registry
  const previousCache = process.env.npm_config_cache
  process.env.npm_config_registry = registry
  process.env.npm_config_cache = path.join(root, "npm-cache")
  return () => {
    if (previousRegistry === undefined) delete process.env.npm_config_registry
    else process.env.npm_config_registry = previousRegistry
    if (previousCache === undefined) delete process.env.npm_config_cache
    else process.env.npm_config_cache = previousCache
  }
}

test(
  "add installs one generation under the upstream cache layout",
  async () => {
    const { root, paths, cleanup } = await workspace()
    const registry = await startRegistry({
      "registry-plugin": { "1.0.0": { "index.js": 'export const version = "1.0.0"\n' } },
    })
    const restore = useNpmEnvironment(registry.url, root)
    try {
      const installed = await npm(paths).add("registry-plugin@latest")
      expect(installed.name).toBe("registry-plugin")
      expect(installed.version).toBe("1.0.0")
      expect(installed.directory.endsWith(path.join("node_modules", "registry-plugin"))).toBe(true)
      expect(installed.directory).toContain(path.join("npm", "registry-plugin@latest"))
      const generations = await readdir(path.join(paths.cache, "npm", "registry-plugin@latest"))
      expect(generations.filter((entry) => /^\d+$/.test(entry))).toHaveLength(1)
      expect(await Bun.file(path.join(installed.directory, "index.js")).text()).toContain("1.0.0")
    } finally {
      restore()
      await registry.stop()
      await cleanup()
    }
  },
  20000,
)

test(
  "add reuses the current generation",
  async () => {
    const { root, paths, cleanup } = await workspace()
    const registry = await startRegistry({
      "registry-plugin": { "1.0.0": { "index.js": 'export const version = "1.0.0"\n' } },
    })
    const restore = useNpmEnvironment(registry.url, root)
    try {
      const first = await npm(paths).add("registry-plugin@latest")
      const second = await npm(paths).add("registry-plugin@latest")
      expect(second.directory).toBe(first.directory)
      expect(second.version).toBe(first.version)
    } finally {
      restore()
      await registry.stop()
      await cleanup()
    }
  },
  20000,
)

test(
  "update installs a newer generation",
  async () => {
    const { root, paths, cleanup } = await workspace()
    const registry = await startRegistry({
      "registry-plugin": { "1.0.0": { "index.js": 'export const version = "1.0.0"\n' } },
    })
    const restore = useNpmEnvironment(registry.url, root)
    try {
      const installed = await npm(paths).add("registry-plugin@latest")
      await registry.publish("registry-plugin", "1.1.0", { "index.js": 'export const version = "1.1.0"\n' })
      const updated = await npm(paths).update("registry-plugin@latest")
      expect(updated.version).toBe("1.1.0")
      expect(updated.directory).not.toBe(installed.directory)
      expect(await Bun.file(path.join(updated.directory, "index.js")).text()).toContain("1.1.0")
    } finally {
      restore()
      await registry.stop()
      await cleanup()
    }
  },
  20000,
)

test(
  "reject writes a rejected marker and resolve falls back to the previous generation",
  async () => {
    const { root, paths, cleanup } = await workspace()
    const registry = await startRegistry({
      "registry-plugin": { "1.0.0": { "index.js": 'export const version = "1.0.0"\n' } },
    })
    const restore = useNpmEnvironment(registry.url, root)
    try {
      await npm(paths).add("registry-plugin@latest")
      await registry.publish("registry-plugin", "1.1.0", { "index.js": 'export const version = "1.1.0"\n' })
      const updated = await npm(paths).update("registry-plugin@latest")
      expect(updated.version).toBe("1.1.0")
      expect(await npm(paths).reject("registry-plugin@latest", "1.1.0")).toBe(true)
      const packageRoot = path.join(paths.cache, "npm", "registry-plugin@latest")
      const generations = (await readdir(packageRoot))
        .filter((entry) => /^\d+$/.test(entry))
        .toSorted((a, b) => Number(a) - Number(b))
      await stat(path.join(packageRoot, `${generations.at(-1)}.rejected`))
      const resolved = await npm(paths).resolve("registry-plugin@latest")
      expect(resolved.version).toBe("1.0.0")
    } finally {
      restore()
      await registry.stop()
      await cleanup()
    }
  },
  20000,
)

test(
  "follows is true only for @latest, ranges and Git branches",
  async () => {
    const { root, paths, cleanup } = await workspace()
    const registry = await startRegistry({
      "registry-plugin": { "1.0.0": { "index.js": 'export const version = "1.0.0"\n' } },
    })
    const restore = useNpmEnvironment(registry.url, root)
    try {
      expect(await npm(paths).follows("registry-plugin@latest")).toBe(true)
      expect(await npm(paths).follows("registry-plugin@^1.0.0")).toBe(true)
      expect(await npm(paths).follows("registry-plugin@~1.0.0")).toBe(true)
      expect(await npm(paths).follows("registry-plugin@1.x")).toBe(true)
      expect(await npm(paths).follows("registry-plugin")).toBe(false)
      expect(await npm(paths).follows("registry-plugin@1.0.0")).toBe(false)
      expect(await npm(paths).follows("registry-plugin@next")).toBe(false)

      const repository = path.join(root, "repository")
      const git = (args: string[], cwd = repository) => {
        const result = Bun.spawnSync(["git", ...args], { cwd })
        if (result.exitCode !== 0) throw new Error(result.stderr.toString())
        return result.stdout.toString().trim()
      }
      const init = Bun.spawnSync(["git", "init", "-q", "-b", "fixture-branch", repository])
      if (init.exitCode !== 0) throw new Error(init.stderr.toString())
      await Bun.write(path.join(repository, "index.js"), 'export const version = "0.0.0"\n')
      git(["add", "index.js"])
      git(["-c", "user.name=Fixture", "-c", "user.email=fixture@example.com", "commit", "-q", "-m", "fixture"])
      git(["tag", "fixture-tag"])
      const commit = git(["rev-parse", "HEAD"])
      const url = `git+${pathToFileURL(repository).href}`
      expect(await npm(paths).follows(`${url}#fixture-branch`)).toBe(true)
      expect(await npm(paths).follows(`${url}#fixture-tag`)).toBe(false)
      expect(await npm(paths).follows(`${url}#${commit}`)).toBe(false)
    } finally {
      restore()
      await registry.stop()
      await cleanup()
    }
  },
  20000,
)

test(
  "the install lock lives under <state>/locks",
  async () => {
    const { root, paths, cleanup } = await workspace()
    const registry = await startRegistry(
      { "registry-plugin": { "1.0.0": { "index.js": 'export const version = "1.0.0"\n' } } },
      { delayMs: 500 },
    )
    const restore = useNpmEnvironment(registry.url, root)
    try {
      const pending = npm(paths).add("registry-plugin@latest")
      const locks = path.join(paths.state, "locks")
      let found = false
      for (let attempt = 0; attempt < 200 && !found; attempt++) {
        const entries = await readdir(locks).catch(() => [] as string[])
        found = entries.some((entry) => entry.endsWith(".lock"))
        if (!found) await Bun.sleep(25)
      }
      expect(found).toBe(true)
      const installed = await pending
      expect(installed.version).toBe("1.0.0")
    } finally {
      restore()
      await registry.stop()
      await cleanup()
    }
  },
  20000,
)
