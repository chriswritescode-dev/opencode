import type { Message } from "./protocol.gen"

const fs = process.getBuiltinModule("node:fs")

export const JSON = 0
export const BINARY = 1
export const DEFAULT_MAX_FRAME = 16 << 20

const HEADER = 5

export type Frame = { message: Message; bytes?: Uint8Array }

export function encode(message: Message, bytes: Uint8Array | undefined, maxFrame: number): Uint8Array {
  const json = Buffer.from(globalThis.JSON.stringify(message))
  const len = bytes === undefined ? json.length : json.length + 4 + bytes.length
  const max = Math.min(maxFrame, 0xffffffff)
  if (len > max) {
    const error = new Error(`a frame of ${len} bytes, over its cap of ${max}`)
    error.name = "TooLarge"
    throw error
  }
  const frame = Buffer.allocUnsafe(len + HEADER)
  frame.writeUInt32LE(len, 0)
  frame[4] = bytes === undefined ? JSON : BINARY
  if (bytes === undefined) {
    frame.set(json, HEADER)
    return frame
  }
  frame.writeUInt32LE(json.length, HEADER)
  frame.set(json, HEADER + 4)
  frame.set(bytes, HEADER + 4 + json.length)
  return frame
}

export class Decoder {
  maxFrame: number
  private chunks: Buffer[] = []
  private size = 0

  constructor(maxFrame: number) {
    this.maxFrame = maxFrame
  }

  push(chunk: Uint8Array): Frame[] {
    if (chunk.length > 0) this.chunks.push(asBuffer(chunk))
    this.size += chunk.length
    const decoded = drain(this.chunks, this.size, this.maxFrame)
    this.size = decoded.size
    return decoded.frames
  }

  end(): void {
    const got = this.size
    if (got === 0) return
    if (got < HEADER) throw new Error(`a frame cut short: ${got} of ${HEADER} bytes`)
    throw new Error(`a frame cut short: ${got} of ${HEADER + collect(this.chunks, HEADER).readUInt32LE(0)} bytes`)
  }
}

function drain(
  chunks: Buffer[],
  size: number,
  maxFrame: number,
): { frames: Frame[]; chunks: Buffer[]; size: number } {
  const frames: Frame[] = []
  for (;;) {
    if (size < HEADER) break
    const header = collect(chunks, HEADER)
    const kind = header[4]
    if (kind !== JSON && kind !== BINARY) throw new Error(`a frame of unknown kind ${kind}`)
    const len = header.readUInt32LE(0)
    if (len > maxFrame) throw new Error(`a frame of kind ${kind} and ${len} bytes, over its cap`)
    if (size < HEADER + len) break
    frames.push(decode(kind, take(chunks, HEADER + len).subarray(HEADER)))
    size -= HEADER + len
  }
  return { frames, chunks, size }
}

function asBuffer(chunk: Uint8Array): Buffer {
  if (Buffer.isBuffer(chunk)) return chunk
  return Buffer.from(chunk.buffer, chunk.byteOffset, chunk.length)
}

function collect(chunks: Buffer[], bytes: number): Buffer {
  const first = chunks[0]
  if (first.length >= bytes) return first.subarray(0, bytes)
  const header = Buffer.allocUnsafe(bytes)
  let at = 0
  for (const chunk of chunks) {
    const copy = Math.min(chunk.length, bytes - at)
    chunk.copy(header, at, 0, copy)
    at += copy
    if (at === bytes) break
  }
  return header
}

function take(chunks: Buffer[], bytes: number): Buffer {
  const first = chunks[0]
  if (first.length >= bytes) {
    if (first.length === bytes) chunks.shift()
    else chunks[0] = first.subarray(bytes)
    return first.subarray(0, bytes)
  }
  const frame = Buffer.allocUnsafe(bytes)
  let at = 0
  while (at < bytes) {
    const chunk = chunks[0]
    const copy = Math.min(chunk.length, bytes - at)
    chunk.copy(frame, at, 0, copy)
    at += copy
    if (copy === chunk.length) chunks.shift()
    else chunks[0] = chunk.subarray(copy)
  }
  return frame
}

function decode(kind: number, payload: Buffer): Frame {
  if (kind === JSON) return { message: parse(payload) }
  if (payload.length < 4) throw new Error("a frame whose JSON did not parse: a binary frame without its JSON length")
  const jsonLength = payload.readUInt32LE(0)
  if (jsonLength > payload.length - 4) {
    throw new Error("a frame whose JSON did not parse: a binary frame whose JSON runs past its payload")
  }
  return {
    message: parse(payload.subarray(4, 4 + jsonLength)),
    bytes: Buffer.from(payload.subarray(4 + jsonLength)),
  }
}

function parse(payload: Uint8Array): Message {
  try {
    return globalThis.JSON.parse(Buffer.from(payload).toString())
  } catch (error) {
    const reason = error instanceof Error ? error.message : String(error)
    throw new Error(`a frame whose JSON did not parse: ${reason}`)
  }
}

export function writer(fd: number): (bytes: Uint8Array) => void {
  const queue: Uint8Array[] = []
  let failure: unknown
  const flush = () => {
    while (queue.length > 0) {
      const head = queue[0]
      try {
        const written = fs.writeSync(fd, head)
        if (written < head.length) queue[0] = head.subarray(written)
        else queue.shift()
      } catch (error) {
        if (error instanceof Error && "code" in error && error.code === "EAGAIN") {
          setTimeout(flush, 1)
          return
        }
        failure = error
        queue.length = 0
        return
      }
    }
  }
  return (bytes) => {
    if (failure !== undefined) throw failure
    queue.push(bytes)
    if (queue.length === 1) flush()
    if (failure !== undefined) throw failure
  }
}
