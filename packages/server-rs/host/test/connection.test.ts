import { expect, test } from "bun:test"

import { Connection } from "../src/connection"
import type { Outcome } from "../src/connection"
import { Decoder } from "../src/frame"
import type { Frame } from "../src/frame"
import type { JsonValue, LocationRef } from "../src/protocol.gen"

const MAX_FRAME = 1 << 20

class Queue implements AsyncIterable<Frame> {
  private items: Frame[] = []
  private waiters: Array<(result: IteratorResult<Frame>) => void> = []
  private closed = false

  push(frame: Frame): void {
    const waiter = this.waiters.shift()
    if (waiter !== undefined) {
      waiter({ value: frame, done: false })
      return
    }
    this.items.push(frame)
  }

  stop(): void {
    this.closed = true
    const waiters = this.waiters
    this.waiters = []
    for (const waiter of waiters) waiter({ done: true, value: undefined })
  }

  [Symbol.asyncIterator](): AsyncIterator<Frame> {
    return {
      next: () => this.next(),
      return: () => {
        this.stop()
        return Promise.resolve({ done: true, value: undefined })
      },
    }
  }

  private next(): Promise<IteratorResult<Frame>> {
    const frame = this.items.shift()
    if (frame !== undefined) return Promise.resolve({ value: frame, done: false })
    if (this.closed) return Promise.resolve({ done: true, value: undefined })
    return new Promise((resolve) => this.waiters.push(resolve))
  }
}

function pair(window = 4) {
  const toA = new Queue()
  const toB = new Queue()
  const a = new Connection(
    {
      write: (bytes) => {
        for (const frame of new Decoder(MAX_FRAME).push(bytes)) toB.push(frame)
      },
      frames: toA,
    },
    { window, maxFrame: MAX_FRAME },
  )
  const b = new Connection(
    {
      write: (bytes) => {
        for (const frame of new Decoder(MAX_FRAME).push(bytes)) toA.push(frame)
      },
      frames: toB,
    },
    { window, maxFrame: MAX_FRAME },
  )
  return { a, b }
}

function isRecord(value: JsonValue): value is { [key in string]: JsonValue } {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function rejection(promise: Promise<Outcome>): Promise<Error> {
  return promise.then(
    () => new Error("the call resolved"),
    (error: Error) => error,
  )
}

test("a call returns a JSON value with its bytes", async () => {
  const { a, b } = pair()
  b.register("echo", async (_ctx, params, bytes) => ({ value: params, bytes }))
  const value = { text: "hello", count: 1 }
  const bytes = new Uint8Array([1, 2, 3, 255])
  const outcome = await a.call("echo", value, { bytes })
  expect(outcome.value).toEqual(value)
  expect(Array.from(outcome.bytes ?? [])).toEqual([1, 2, 3, 255])
  a.close()
  b.close()
})

test("a nested call keeps its parent's location", async () => {
  const { a, b } = pair()
  const location: LocationRef = { directory: "/tmp/outer", workspaceID: "workspace-1" }
  let seen: LocationRef | undefined
  b.register("inner", async (ctx, params) => {
    seen = ctx.location
    return { value: params }
  })
  a.register("outer", (ctx, params) => ctx.call("inner", params))
  const outcome = await b.call("outer", { nested: true }, { location })
  expect(outcome.value).toEqual({ nested: true })
  expect(seen).toEqual(location)
  a.close()
  b.close()
})

test("aborting a caller's signal aborts the callee and rejects with AbortError", async () => {
  const { a, b } = pair()
  let entered = () => {}
  const started = new Promise<void>((resolve) => {
    entered = resolve
  })
  let stopped = () => {}
  const aborted = new Promise<void>((resolve) => {
    stopped = resolve
  })
  b.register("wait", (ctx) => {
    entered()
    ctx.signal.addEventListener("abort", stopped)
    return new Promise<Outcome>((resolve) => {
      ctx.signal.addEventListener("abort", () => resolve({ value: null }))
    })
  })
  const controller = new AbortController()
  const pending = a.call("wait", null, { signal: controller.signal })
  await started
  controller.abort()
  const failure = await rejection(pending)
  expect(failure.name).toBe("AbortError")
  await aborted
  a.close()
  b.close()
})

test("a stream respects its window and keeps chunk order", async () => {
  const { a, b } = pair(2)
  let produced = 0
  let consumed = 0
  let worst = 0
  b.registerStream("count", async function* () {
    for (let index = 0; index < 10; index++) {
      produced += 1
      if (produced - consumed > 2) throw new Error(`the handler produced ${produced - consumed} chunks ahead`)
      yield { value: { index } }
    }
  })
  const seen: number[] = []
  for await (const item of a.stream("count", null)) {
    if (!isRecord(item.value)) throw new Error("a chunk without a record value")
    const index = item.value.index
    if (typeof index !== "number") throw new Error("a chunk without an index")
    seen.push(index)
    consumed += 1
    worst = Math.max(worst, produced - consumed)
  }
  expect(seen).toEqual([0, 1, 2, 3, 4, 5, 6, 7, 8, 9])
  expect(worst).toBeLessThanOrEqual(2)
  expect(b.stats.maxInflight).toBe(2)
  a.close()
  b.close()
})

test("an error in a stream throws at the consumer", async () => {
  const { a, b } = pair()
  b.registerStream("fail", async function* () {
    yield { value: { index: 0 } }
    throw new RangeError("stream failed")
  })
  const seen: JsonValue[] = []
  const failure = await (async () => {
    try {
      for await (const item of a.stream("fail", null)) seen.push(item.value)
      return new Error("the stream ended")
    } catch (error) {
      return error instanceof Error ? error : new Error(String(error))
    }
  })()
  expect(seen).toEqual([{ index: 0 }])
  expect(failure.name).toBe("RangeError")
  expect(failure.message).toBe("stream failed")
  a.close()
  b.close()
})

test("an unknown method rejects with UnknownMethod", async () => {
  const { a, b } = pair()
  const failure = await rejection(a.call("missing", null))
  expect(failure.name).toBe("UnknownMethod")
  expect(failure.message).toContain("missing")
  a.close()
  b.close()
})

test("a handler that throws rejects with the thrown error name", async () => {
  const { a, b } = pair()
  b.register("boom", async () => {
    throw new RangeError("too far")
  })
  const failure = await rejection(a.call("boom", null))
  expect(failure.name).toBe("RangeError")
  expect(failure.message).toBe("too far")
  a.close()
  b.close()
})
