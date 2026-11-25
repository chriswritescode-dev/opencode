import { rmSync } from "node:fs"
import fs from "node:fs/promises"
import path from "node:path"

export type Sandbox = { root: string; work: string }

const roots = new Set<string>()

process.on("exit", () => roots.forEach((root) => rmSync(root, { recursive: true, force: true, maxRetries: 3 })))

export async function createSandbox(parent?: string): Promise<Sandbox> {
  const base = parent ?? process.env.PARITY_SANDBOX_DIR ?? path.join("/tmp", "opencode-parity")
  await fs.mkdir(base, { recursive: true })
  const root = await fs.mkdtemp(path.join(base, "opencode-parity-"))
  roots.add(root)
  await resetSandbox(root)
  return { root, work: path.join(root, "work") }
}

export async function removeSandbox(root: string): Promise<void> {
  await fs.rm(root, { recursive: true, force: true })
  roots.delete(root)
}

export async function resetSandbox(root: string): Promise<void> {
  await fs.rm(root, { recursive: true, force: true })
  await Promise.all(
    ["home", "home/.config", "home/.local/share", "home/.local/state", "home/.cache", "tmp", "work"].map((dir) =>
      fs.mkdir(path.join(root, dir), { recursive: true }),
    ),
  )
}

export async function sandboxEnv(
  root: string,
  extra: Record<string, string> = {},
  config: Record<string, unknown> = {},
  modelURL: string,
  models: unknown = {},
): Promise<Record<string, string>> {
  const home = path.join(root, "home")
  const bunCache = path.join(path.dirname(root), "bun-cache")
  await Bun.write(path.join(root, "models.json"), JSON.stringify(models))
  await fs.mkdir(bunCache, { recursive: true })
  return {
    PATH: process.env.PATH ?? "/usr/bin:/bin",
    LANG: "C.UTF-8",
    HOME: home,
    OPENCODE_TEST_HOME: home,
    XDG_CONFIG_HOME: path.join(home, ".config"),
    XDG_DATA_HOME: path.join(home, ".local/share"),
    XDG_STATE_HOME: path.join(home, ".local/state"),
    XDG_CACHE_HOME: path.join(home, ".cache"),
    TMPDIR: path.join(root, "tmp"),
    OPENCODE_PASSWORD: "parity",
    OPENCODE_DISABLE_AUTOUPDATE: "1",
    OPENCODE_DISABLE_MODELS_FETCH: "1",
    OPENCODE_MODELS_PATH: path.join(root, "models.json"),
    OPENCODE_DISABLE_FILEWATCHER: "1",
    OPENCODE_DISABLE_CHANNEL_DB: "1",
    OPENCODE_FLAGS: "-full_codemode",
    OPENCODE_CONFIG_CONTENT: JSON.stringify(
      deepMerge(
        {
          update: "disable",
          model: "openai-test/test-model",
          agents: { title: { disabled: true } },
          providers: {
            "openai-test": modelProvider("OpenAI Test", "@ai-sdk/openai-compatible", modelURL),
            "anthropic-test": modelProvider("Anthropic Test", "@ai-sdk/anthropic", modelURL),
          },
        },
        config,
      ),
    ),
    BUN_RUNTIME_TRANSPILER_CACHE_PATH: bunCache,
    ...extra,
  }
}

function modelProvider(name: string, pkg: string, modelURL: string) {
  return {
    name,
    package: pkg,
    settings: { apiKey: "parity", baseURL: `${modelURL}/v1` },
    models: { "test-model": testModel },
  }
}

const testModel = {
  name: "Test Model",
  capabilities: { tools: true, input: ["text"], output: ["text"] },
  cost: { input: 0, output: 0 },
  limit: { context: 100_000, output: 10_000 },
}

function deepMerge(base: Record<string, unknown>, override: Record<string, unknown>): Record<string, unknown> {
  const merged: Record<string, unknown> = { ...base }
  for (const [key, value] of Object.entries(override)) {
    const current = merged[key]
    merged[key] = isPlainObject(current) && isPlainObject(value) ? deepMerge(current, value) : value
  }
  return merged
}

function isPlainObject(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}
