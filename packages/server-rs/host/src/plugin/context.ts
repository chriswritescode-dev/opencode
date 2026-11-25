import { Effect } from "effect"
import type { Context } from "@opencode/plugin/effect/plugin"

export type PluginList = () => Promise<unknown>

type Unavailable = {
  (...args: unknown[]): Effect.Effect<never>
  readonly [key: string]: Unavailable
}

function unavailable(path: string): Unavailable {
  const callable = () => Effect.die(new Error(`${path} is not available in the sidecar yet (OR-63)`))
  return new Proxy(callable as unknown as Unavailable, {
    get: (_target, property) => (typeof property === "string" ? unavailable(`${path}.${property}`) : undefined),
  })
}

export function makeContext(input: {
  app: Context["app"]
  location: unknown
  options: Context["options"]
  list: PluginList
}): Context {
  const context = {
    app: input.app,
    location: input.location,
    options: input.options,
    plugin: { list: () => Effect.promise(input.list) },
    agent: unavailable("agent"),
    aisdk: unavailable("aisdk"),
    command: unavailable("command"),
    credential: unavailable("credential"),
    event: unavailable("event"),
    experimental: unavailable("experimental"),
    generate: unavailable("generate"),
    integration: unavailable("integration"),
    mcp: unavailable("mcp"),
    model: unavailable("model"),
    permission: unavailable("permission"),
    provider: unavailable("provider"),
    reference: unavailable("reference"),
    rpc: unavailable("rpc"),
    session: unavailable("session"),
    shell: unavailable("shell"),
    skill: unavailable("skill"),
    storage: unavailable("storage"),
    tool: unavailable("tool"),
    vcs: unavailable("vcs"),
    websearch: unavailable("websearch"),
    worktree: unavailable("worktree"),
  }
  return context as unknown as Context
}
