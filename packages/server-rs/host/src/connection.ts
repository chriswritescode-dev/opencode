import { encode } from "./frame"
import type { Frame } from "./frame"
import type { JsonValue, LocationRef, Message } from "./protocol.gen"

export type Outcome = { value: JsonValue; bytes?: Uint8Array }
export type CallOptions = { location?: LocationRef; bytes?: Uint8Array; signal?: AbortSignal }
export type Context = {
  id: number
  location?: LocationRef
  signal: AbortSignal
  call(method: string, params: JsonValue, options?: CallOptions): Promise<Outcome>
  stream(method: string, params: JsonValue, options?: CallOptions): AsyncIterable<Outcome>
}
export type Handler = (ctx: Context, params: JsonValue, bytes?: Uint8Array) => Promise<Outcome>
export type StreamHandler = (ctx: Context, params: JsonValue, bytes?: Uint8Array) => AsyncIterable<Outcome>
export type Transport = {
  write: (bytes: Uint8Array) => void
  frames: AsyncIterable<Frame>
}
export type Stats = { maxInflight: number; maxBuffered: number }

type CallMessage = Extract<Message, { type: "call" }>

type PendingCall = {
  kind: "call"
  resolve: (outcome: Outcome) => void
  reject: (error: Error) => void
  detach: () => void
}

type PendingStream = {
  kind: "stream"
  id: number
  queue: Outcome[]
  waiters: Array<() => void>
  closed: boolean
  ended: boolean
  failure?: Error
  detach: () => void
}

type Pending = PendingCall | PendingStream

type Incoming = {
  controller: AbortController
  credit: number
  sent: number
  credited: number
  granted?: number
  wake?: () => void
}

export class Connection {
  readonly done: Promise<void>
  readonly stats: Stats = { maxInflight: 0, maxBuffered: 0 }
  onControl: (message: Message) => void = () => {}

  private readonly transport: Transport
  private readonly window: number
  private readonly maxFrame: number
  private readonly handlers = new Map<string, Handler>()
  private readonly streams = new Map<string, StreamHandler>()
  private readonly pending = new Map<number, Pending>()
  private readonly incoming = new Map<number, Incoming>()
  private nextID = 1
  private closed: Error | undefined
  private finish: () => void = () => {}

  constructor(transport: Transport, options: { window: number; maxFrame: number }) {
    this.transport = transport
    this.window = options.window
    this.maxFrame = options.maxFrame
    this.done = new Promise<void>((resolve) => {
      this.finish = resolve
    })
    this.read().catch(() => undefined)
  }

  register(method: string, handler: Handler): void {
    this.handlers.set(method, handler)
  }

  registerStream(method: string, handler: StreamHandler): void {
    this.streams.set(method, handler)
  }

  send(message: Message): void {
    this.write(message, undefined)
  }

  call(method: string, params: JsonValue, options?: CallOptions): Promise<Outcome> {
    return this.invoke(method, params, options, undefined)
  }

  stream(method: string, params: JsonValue, options?: CallOptions): AsyncIterable<Outcome> {
    return this.invokeStream(method, params, options, undefined)
  }

  close(error?: Error): void {
    if (this.closed !== undefined) return
    const reason = error ?? new Error("connection closed")
    this.closed = reason
    for (const pending of this.pending.values()) {
      pending.detach()
      if (pending.kind === "call") {
        pending.reject(reason)
        continue
      }
      pending.closed = true
      pending.failure = reason
      pending.queue = []
      wake(pending)
    }
    this.pending.clear()
    for (const incoming of this.incoming.values()) {
      incoming.controller.abort()
      release(incoming)
    }
    this.incoming.clear()
    this.finish()
  }

  private invoke(
    method: string,
    params: JsonValue,
    options: CallOptions | undefined,
    parent: number | undefined,
  ): Promise<Outcome> {
    const id = this.nextID++
    const signal = options?.signal
    return new Promise<Outcome>((resolve, reject) => {
      const pending: PendingCall = { kind: "call", resolve, reject, detach: () => {} }
      const abort = () => {
        if (this.pending.get(id) !== pending) return
        this.pending.delete(id)
        pending.detach()
        this.quiet({ type: "cancel", id })
        reject(abortError())
      }
      this.pending.set(id, pending)
      if (signal !== undefined) {
        pending.detach = () => signal.removeEventListener("abort", abort)
        signal.addEventListener("abort", abort)
      }
      try {
        this.write(callMessage(id, method, params, options, parent, false), options?.bytes)
      } catch (thrown) {
        this.pending.delete(id)
        pending.detach()
        reject(thrown instanceof Error ? thrown : new Error(String(thrown)))
        return
      }
      if (signal?.aborted === true) abort()
    })
  }

  private invokeStream(
    method: string,
    params: JsonValue,
    options: CallOptions | undefined,
    parent: number | undefined,
  ): AsyncIterable<Outcome> {
    const id = this.nextID++
    const signal = options?.signal
    const entry: PendingStream = {
      kind: "stream",
      id,
      queue: [],
      waiters: [],
      closed: false,
      ended: false,
      detach: () => {},
    }
    const abort = () => {
      if (this.pending.get(id) !== entry) return
      this.pending.delete(id)
      entry.detach()
      this.quiet({ type: "cancel", id })
      entry.closed = true
      entry.failure = abortError()
      entry.queue = []
      wake(entry)
    }
    this.pending.set(id, entry)
    if (signal !== undefined) {
      entry.detach = () => signal.removeEventListener("abort", abort)
      signal.addEventListener("abort", abort)
    }
    try {
      this.write(callMessage(id, method, params, options, parent, true), options?.bytes)
      this.send({ type: "credit", id, chunks: this.window })
    } catch (thrown) {
      this.pending.delete(id)
      entry.detach()
      entry.closed = true
      entry.queue = []
      entry.failure = thrown instanceof Error ? thrown : new Error(String(thrown))
      wake(entry)
    }
    if (signal?.aborted === true) abort()
    return {
      [Symbol.asyncIterator]: () => ({
        next: () => this.nextStream(entry),
        return: () => this.stopStream(entry),
      }),
    }
  }

  private async nextStream(entry: PendingStream): Promise<IteratorResult<Outcome>> {
    for (;;) {
      const value = entry.queue.shift()
      if (value !== undefined) {
        if (!entry.ended) this.quiet({ type: "credit", id: entry.id, chunks: 1 })
        return { value, done: false }
      }
      if (entry.failure !== undefined) throw entry.failure
      if (entry.closed) return { done: true, value: undefined }
      await new Promise<void>((resolve) => entry.waiters.push(resolve))
    }
  }

  private stopStream(entry: PendingStream): Promise<IteratorResult<Outcome>> {
    if (this.pending.get(entry.id) === entry) {
      this.pending.delete(entry.id)
      this.quiet({ type: "cancel", id: entry.id })
    }
    entry.detach()
    entry.closed = true
    entry.queue = []
    wake(entry)
    return Promise.resolve({ done: true, value: undefined })
  }

  private async read(): Promise<void> {
    try {
      for await (const frame of this.transport.frames) {
        if (this.closed !== undefined) return
        this.receive(frame)
      }
    } catch (thrown) {
      this.close(thrown instanceof Error ? thrown : new Error(String(thrown)))
      return
    }
    this.close()
  }

  private receive(frame: Frame): void {
    const message = frame.message
    if (message.type === "call") {
      this.dispatch(message, frame.bytes)
      return
    }
    if (message.type === "result") {
      const pending = this.pending.get(message.id)
      if (pending === undefined || pending.kind !== "call") return
      this.pending.delete(message.id)
      pending.detach()
      pending.resolve(outcomeOf(message.value, frame.bytes))
      return
    }
    if (message.type === "error") {
      const pending = this.pending.get(message.id)
      if (pending === undefined) return
      this.pending.delete(message.id)
      pending.detach()
      if (pending.kind === "call") {
        pending.reject(remoteError(message.error._tag, message.error.message))
        return
      }
      pending.closed = true
      pending.ended = true
      pending.failure = remoteError(message.error._tag, message.error.message)
      wake(pending)
      return
    }
    if (message.type === "chunk") {
      const pending = this.pending.get(message.id)
      if (pending === undefined || pending.kind !== "stream" || pending.closed) return
      pending.queue.push(outcomeOf(message.value, frame.bytes))
      this.stats.maxBuffered = Math.max(this.stats.maxBuffered, pending.queue.length)
      wake(pending)
      return
    }
    if (message.type === "end") {
      const pending = this.pending.get(message.id)
      if (pending === undefined || pending.kind !== "stream") return
      this.pending.delete(message.id)
      pending.detach()
      pending.closed = true
      pending.ended = true
      wake(pending)
      return
    }
    if (message.type === "credit") {
      const incoming = this.incoming.get(message.id)
      if (incoming === undefined) return
      if (incoming.granted === undefined) incoming.granted = message.chunks
      else incoming.credited += message.chunks
      incoming.credit += message.chunks
      release(incoming)
      return
    }
    if (message.type === "cancel") {
      const incoming = this.incoming.get(message.id)
      if (incoming === undefined) return
      this.incoming.delete(message.id)
      incoming.controller.abort()
      release(incoming)
      return
    }
    this.onControl(message)
  }

  private dispatch(message: CallMessage, bytes: Uint8Array | undefined): void {
    const incoming: Incoming = { controller: new AbortController(), credit: 0, sent: 0, credited: 0 }
    this.incoming.set(message.id, incoming)
    const run = message.stream ? this.runStream(message, bytes, incoming) : this.runCall(message, bytes, incoming)
    run.catch(() => this.dropIncoming(message.id))
  }

  private async runCall(message: CallMessage, bytes: Uint8Array | undefined, incoming: Incoming): Promise<void> {
    const handler = this.handlers.get(message.method)
    if (handler === undefined) {
      this.fail(message.id, incoming, "UnknownMethod", `an unknown method ${message.method}`)
      this.dropIncoming(message.id)
      return
    }
    try {
      const outcome = await handler(this.context(message, incoming.controller.signal), message.params, bytes)
      if (incoming.controller.signal.aborted) return
      this.write({ type: "result", id: message.id, value: outcome.value ?? null }, outcome.bytes)
    } catch (thrown) {
      this.fail(
        message.id,
        incoming,
        thrown instanceof Error && thrown.name ? thrown.name : "Error",
        thrown instanceof Error ? thrown.message : String(thrown),
      )
    } finally {
      this.dropIncoming(message.id)
    }
  }

  private async runStream(message: CallMessage, bytes: Uint8Array | undefined, incoming: Incoming): Promise<void> {
    const handler = this.streams.get(message.method)
    if (handler === undefined) {
      this.fail(message.id, incoming, "UnknownMethod", `an unknown method ${message.method}`)
      this.dropIncoming(message.id)
      return
    }
    let iterator: AsyncIterator<Outcome> | undefined
    try {
      const iterable = handler(this.context(message, incoming.controller.signal), message.params, bytes)
      iterator = iterable[Symbol.asyncIterator]()
      for (;;) {
        while (incoming.credit <= 0) {
          if (incoming.controller.signal.aborted) return
          await new Promise<void>((resolve) => {
            incoming.wake = resolve
          })
          incoming.wake = undefined
        }
        if (incoming.controller.signal.aborted) return
        const next = await iterator.next()
        if (incoming.controller.signal.aborted) return
        if (next.done === true) break
        incoming.credit -= 1
        incoming.sent += 1
        this.stats.maxInflight = Math.max(this.stats.maxInflight, incoming.sent - incoming.credited)
        this.write({ type: "chunk", id: message.id, value: next.value.value ?? null }, next.value.bytes)
      }
      if (!incoming.controller.signal.aborted) this.quiet({ type: "end", id: message.id })
    } catch (thrown) {
      this.fail(
        message.id,
        incoming,
        thrown instanceof Error && thrown.name ? thrown.name : "Error",
        thrown instanceof Error ? thrown.message : String(thrown),
      )
    } finally {
      await Promise.resolve(iterator?.return?.()).catch(() => undefined)
      this.dropIncoming(message.id)
    }
  }

  private context(message: CallMessage, signal: AbortSignal): Context {
    return {
      id: message.id,
      location: message.location,
      signal,
      call: (method, params, options) => this.invoke(method, params, inherit(options, message.location), message.id),
      stream: (method, params, options) =>
        this.invokeStream(method, params, inherit(options, message.location), message.id),
    }
  }

  private fail(id: number, incoming: Incoming, tag: string, message: string): void {
    if (incoming.controller.signal.aborted) return
    this.quiet({ type: "error", id, error: { _tag: tag, message } })
  }

  private dropIncoming(id: number): void {
    this.incoming.delete(id)
  }

  private write(message: Message, bytes: Uint8Array | undefined): void {
    this.transport.write(encode(message, bytes, this.maxFrame))
  }

  private quiet(message: Message): void {
    try {
      this.send(message)
    } catch {}
  }
}

function callMessage(
  id: number,
  method: string,
  params: JsonValue,
  options: CallOptions | undefined,
  parent: number | undefined,
  stream: boolean,
): Message {
  return { type: "call", id, method, params, stream, location: options?.location, parent }
}

function inherit(options: CallOptions | undefined, location: LocationRef | undefined): CallOptions | undefined {
  if (location === undefined || options?.location !== undefined) return options
  return { location, bytes: options?.bytes, signal: options?.signal }
}

function outcomeOf(value: JsonValue, bytes: Uint8Array | undefined): Outcome {
  if (bytes === undefined) return { value }
  return { value, bytes }
}

function abortError(): Error {
  const error = new Error("aborted")
  error.name = "AbortError"
  return error
}

function remoteError(tag: string, message: string): Error {
  const error = new Error(message)
  error.name = tag
  return error
}

function wake(entry: PendingStream): void {
  const waiters = entry.waiters
  entry.waiters = []
  for (const waiter of waiters) waiter()
}

function release(incoming: Incoming): void {
  const wakeIncoming = incoming.wake
  incoming.wake = undefined
  wakeIncoming?.()
}
