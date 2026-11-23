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
  if (len > max) throw new Error(`a frame of ${len} bytes, over its cap of ${max}`)
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
  private buffer = Buffer.alloc(0)

  constructor(maxFrame: number) {
    this.maxFrame = maxFrame
  }

  push(chunk: Uint8Array): Frame[] {
    const joined = this.buffer.length === 0 ? Buffer.from(chunk) : Buffer.concat([this.buffer, chunk])
    const frames: Frame[] = []
    let at = 0
    while (joined.length - at >= HEADER) {
      const len = joined.readUInt32LE(at)
      const kind = joined[at + 4]
      if (kind !== JSON && kind !== BINARY) throw new Error(`a frame of unknown kind ${kind}`)
      if (len > this.maxFrame) throw new Error(`a frame of kind ${kind} and ${len} bytes, over its cap`)
      if (joined.length - at < HEADER + len) break
      frames.push(decode(kind, joined.subarray(at + HEADER, at + HEADER + len)))
      at += HEADER + len
    }
    this.buffer = at === joined.length ? Buffer.alloc(0) : Buffer.from(joined.subarray(at))
    return frames
  }

  end(): void {
    const got = this.buffer.length
    if (got === 0) return
    if (got < HEADER) throw new Error(`a frame cut short: ${got} of ${HEADER} bytes`)
    throw new Error(`a frame cut short: ${got} of ${HEADER + this.buffer.readUInt32LE(0)} bytes`)
  }
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

export function write(bytes: Uint8Array, fd = 0): void {
  let at = 0
  while (at < bytes.length) {
    try {
      at += fs.writeSync(fd, bytes, at)
    } catch (error) {
      if (!(error instanceof Error && "code" in error && error.code === "EAGAIN")) throw error
    }
  }
}
