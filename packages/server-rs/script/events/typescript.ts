import { Database as BunDatabase } from "bun:sqlite"
import { mkdtemp, rm } from "node:fs/promises"
import { tmpdir } from "node:os"
import { join } from "node:path"
import { pathToFileURL } from "node:url"
import { dump } from "../storage/typescript"
import { upstreamCoreTree } from "../upstream"

export type Settings = {
  partRows: boolean
  mediaBlob: boolean
  forkMedia: boolean
}

export type PublishStep = {
  kind: "publish"
  id: string
  created: number
  type: string
  data: Record<string, unknown>
  metadata?: Record<string, unknown>
  location?: { directory: string; workspaceID?: string }
}

export type ReplayStep = {
  kind: "replay"
  id: string
  created: number
  seq: number
  aggregateID: string
  type: string
  data: Record<string, unknown>
  publish?: boolean
}

export type Step = PublishStep | ReplayStep

export type Fixture = {
  name: string
  group: string
  persist?: boolean
  partRows?: boolean
  mediaBlob?: boolean
  forkMedia?: boolean
  start?: string[]
  steps: Step[]
  logAfter?: number[]
  filters?: string[][]
}

export type FixtureRecord = {
  name: string
  group: string
  settings: Settings
  persist: boolean
  start: string[]
  steps: Step[]
  logAfter: number[]
  dump: unknown
  frames: Record<string, string[]> | null
  usage: string[] | null
  log: Record<string, Record<string, string>> | null
  error: string | null
}

type BusEvent = { type: string; data: unknown }

type SessionLogHandlers = { handle: (name: string, handler: () => unknown) => unknown }

const tree = await upstreamCoreTree()
const resolve = (name: string) => import(pathToFileURL(Bun.resolveSync(name, `${tree}/packages/core/src`)).href)
const load = (path: string) => import(pathToFileURL(`${tree}/packages/${path}`).href)

const effect = await resolve("effect")
const { Effect, Layer, Clock, References, Stream } = effect
const { Database } = await load("core/src/database/database.ts")
const { Bus } = await load("core/src/bus.ts")
const { Flag } = await load("core/src/flag.ts")
const { SessionBlob } = await load("core/src/session/blob.ts")
const { SessionProjector } = await load("core/src/session/projector.ts")
const { SessionEvent } = await load("core/src/session/event.ts")
const { Global } = await load("util/src/global.ts")
const { LayerNode } = await load("util/src/effect/layer-node.ts")
const drizzle = await resolve("drizzle-orm")
const { ClientApi } = await load("protocol/src/client.ts")
const { Authorization } = await load("protocol/src/middleware/authorization.ts")
const { SchemaErrorMiddleware } = await load("protocol/src/middleware/schema-error.ts")
const { isOpenCodeEvent } = await load("protocol/src/groups/event.ts")
const { HttpRouter, HttpServer } = await import(
  pathToFileURL(Bun.resolveSync("effect/http", `${tree}/packages/core/src`)).href
)
const { HttpApi, HttpApiBuilder, HttpApiGroup, HttpApiMiddleware } = await import(
  pathToFileURL(Bun.resolveSync("effect/http-api", `${tree}/packages/core/src`)).href
)

const LogApi = HttpApi.make("log").add(
  HttpApiGroup.make("server.session").add(ClientApi.groups["server.session"].endpoints["session.log"]),
)

const schemaErrorLayer = HttpApiMiddleware.layerSchemaErrorTransform(SchemaErrorMiddleware, (error: unknown) =>
  Effect.die(error),
)

let currentItems: unknown[] = []
const logEndpoint = HttpRouter.toWebHandler(
  HttpApiBuilder.layer(LogApi).pipe(
    Layer.provide(
      HttpApiBuilder.group(LogApi, "server.session", (handlers: SessionLogHandlers) =>
        handlers.handle("session.log", () => Effect.succeed(Stream.fromIterable(currentItems))),
      ),
    ),
    Layer.provide(schemaErrorLayer),
    Layer.provide(Layer.succeed(Authorization, Authorization.of((value: unknown) => value))),
    Layer.provide(HttpServer.layerServices),
  ),
  { disableLogger: true },
).handler

const baseMasks: Record<string, string[]> = {
  migration: ["time_completed"],
  session_message: ["time_updated"],
  blob: ["time_created"],
}

export function masksFor(fixtures: Fixture[]): Record<string, string[]> {
  const forked = fixtures.some((fixture) => fixture.steps.some((step) => step.type === "session.forked"))
  return forked ? { ...baseMasks, session_v2: ["slug"] } : baseMasks
}

function fixtureClock(time: () => number) {
  return {
    currentTimeMillisUnsafe: () => time(),
    currentTimeMillis: Effect.sync(() => time()),
    monotonicTimeNanosUnsafe: () => BigInt(time()) * 1_000_000n,
    monotonicTimeNanos: Effect.sync(() => BigInt(time()) * 1_000_000n),
    currentTimeNanosUnsafe: () => BigInt(time()) * 1_000_000n,
    currentTimeNanos: Effect.sync(() => BigInt(time()) * 1_000_000n),
    sleep: () => Effect.void,
  }
}

export async function runFixtures(fixtures: Fixture[]): Promise<FixtureRecord[]> {
  const masks = masksFor(fixtures)
  const records: FixtureRecord[] = []
  for (const fixture of fixtures) records.push(await runFixture(fixture, masks))
  return records
}

async function runFixture(fixture: Fixture, masks: Record<string, string[]>): Promise<FixtureRecord> {
  const directory = await mkdtemp(join(tmpdir(), "opencode-events-corpus-"))
  const settings: Settings = {
    partRows: fixture.partRows ?? true,
    mediaBlob: fixture.mediaBlob ?? false,
    forkMedia: fixture.forkMedia ?? false,
  }
  const steps = fixture.steps.map((step): Step =>
    step.kind === "replay" ? { ...step, publish: step.publish ?? true } : step,
  )
  const record = (fields: Pick<FixtureRecord, "dump" | "frames" | "usage" | "log" | "error">): FixtureRecord => ({
    name: fixture.name,
    group: fixture.group,
    settings,
    persist: fixture.persist ?? true,
    start: fixture.start ?? [],
    steps,
    logAfter: fixture.logAfter ?? [],
    ...fields,
  })
  try {
    const path = join(directory, "opencode.db")
    let now = 0
    const filters = fixture.filters ?? [["session.created"]]
    const subscribers = [undefined, ...filters]
    const keys = subscribers.map((filter) => (filter ? filter.join(",") : "all"))
    const frames = new Map<string, string[]>(keys.map((key) => [key, []]))
    const usage: unknown[] = []
    let expectedUsage = 0
    currentItems = []

    const program = Effect.gen(function* () {
      const bus = yield* Bus.Service
      const db = (yield* Database.Service).db
      for (const statement of fixture.start ?? []) yield* db.run(drizzle.sql.raw(statement))
      yield* bus.listen((event: BusEvent) =>
        Effect.sync(() => {
          if (event.type !== "session.usage.updated") return
          usage.push(event.data)
          expectedUsage += 1
        }),
      )
      for (const [index, filter] of subscribers.entries()) {
        yield* bus.listen((event: BusEvent) =>
          Effect.sync(() => {
            if (!isOpenCodeEvent(event)) return
            if (filter && !filter.includes(event.type)) return
            if (event.type === "session.usage.updated") return
            frames.get(keys[index]!)!.push(`data: ${JSON.stringify(event)}\n\n`)
          }),
        )
      }
      const sessions: string[] = []
      for (const step of fixture.steps) {
        now = step.created
        if (step.kind === "publish") {
          const definition = (SessionEvent.All.cases as Record<string, unknown>)[step.type]
          if (!definition) throw new Error(`unknown event type ${step.type}`)
          yield* bus.publish(definition, step.data, {
            id: step.id,
            ...(step.metadata ? { metadata: step.metadata } : {}),
            ...(step.location ? { location: step.location } : {}),
          })
          const sessionID = (step.data as { sessionID?: unknown }).sessionID
          if (typeof sessionID === "string" && !sessions.includes(sessionID)) sessions.push(sessionID)
        } else {
          yield* bus.replay(
            {
              id: step.id,
              created: step.created,
              seq: step.seq,
              aggregateID: step.aggregateID,
              type: step.type,
              data: step.data,
            },
            { publish: step.publish ?? true },
          )
          if (!sessions.includes(step.aggregateID)) sessions.push(step.aggregateID)
        }
        yield* Effect.gen(function* () {
          for (let index = 0; index < 1000; index++) {
            if (usage.length >= expectedUsage) return
            yield* Effect.yieldNow
          }
          throw new Error("fixture did not quiesce")
        })
      }
      const log: Record<string, Record<string, string>> = {}
      for (const sessionID of sessions) {
        log[sessionID] = {}
        for (const after of [undefined, ...(fixture.logAfter ?? [])]) {
          const collected = yield* bus
            .log({ aggregateID: sessionID, ...(after === undefined ? {} : { after }) })
            .pipe(Stream.runCollect)
          currentItems = Array.from(collected as Iterable<{ type: string }>).filter(
            (item) => item.type === "log.synced" || Object.hasOwn(SessionEvent.Durable.cases, item.type),
          )
          const response = yield* Effect.promise(() =>
            Promise.resolve(
              logEndpoint(new Request(`http://opencode.local/api/experimental/session/${sessionID}/log`)),
            ),
          )
          log[sessionID]![after === undefined ? "-1" : String(after)] = yield* Effect.promise(() => response.text())
        }
      }
      return log
    })

    const root = LayerNode.compile(LayerNode.group([Global.node, Database.node, Bus.node, SessionProjector.node]), {
      replacements: [
        Global.node.replace(
          Global.layerWith({ data: directory, config: directory, state: directory, tmp: directory }),
        ),
        Database.node.replace(Database.configured({ path })),
        Bus.node.replace(Bus.configured({ persist: fixture.persist ?? true })),
      ],
    })
    const flags =
      settings.partRows === false || settings.mediaBlob
        ? Flag.testLayer([
            {
              type: "document",
              info: {
                experimental: {
                  ...(settings.partRows === false ? { part_rows: false } : {}),
                  ...(settings.mediaBlob ? { media_blob: true } : {}),
                },
              },
            },
          ])
        : undefined

    const log: Record<string, Record<string, string>> = await Effect.runPromise(
      program.pipe(
        Effect.provide(flags ? Layer.merge(root, flags) : root),
        Effect.provideService(Clock.Clock, fixtureClock(() => now)),
        Effect.provideService(References.MinimumLogLevel, "None"),
        ...(settings.forkMedia ? [Effect.provideService(SessionBlob.ForkMedia, true)] : []),
        Effect.scoped,
      ),
    )

    const opened = new BunDatabase(path)
    const expected = dump(opened, masks)
    opened.close()

    return record({
      dump: expected,
      frames: Object.fromEntries(frames),
      usage: usage.map((data) => JSON.stringify(data)),
      log,
      error: null,
    })
  } catch (caught) {
    return record({
      dump: null,
      frames: null,
      usage: null,
      log: null,
      error: caught instanceof Error ? caught.message : String(caught),
    })
  } finally {
    await rm(directory, { recursive: true, force: true })
  }
}
