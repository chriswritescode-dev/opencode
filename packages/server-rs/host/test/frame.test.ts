import { expect, test } from "bun:test"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"

import { Decoder, DEFAULT_MAX_FRAME, encode, JSON, writer } from "../src/frame"
import type { Frame } from "../src/frame"
import type { Message } from "../src/protocol.gen"

function normalize(frames: Frame[]) {
  return frames.map((frame) => ({
    message: frame.message,
    bytes: frame.bytes === undefined ? undefined : Array.from(frame.bytes),
  }))
}

function header(len: number, kind: number) {
  const bytes = Buffer.alloc(5)
  bytes.writeUInt32LE(len, 0)
  bytes[4] = kind
  return bytes
}

test("round trips a JSON frame", () => {
  const message: Message = { type: "ping", seq: 7 }
  const encoded = encode(message, undefined, DEFAULT_MAX_FRAME)
  expect(encoded.length).toBe(5 + Buffer.byteLength(globalThis.JSON.stringify(message)))
  expect(new Decoder(DEFAULT_MAX_FRAME).push(encoded)).toEqual([{ message }])
})

test("round trips a binary frame", () => {
  const message: Message = { type: "chunk", id: 3, value: { text: "hello" } }
  const bytes = new Uint8Array([0, 1, 2, 255])
  const frames = new Decoder(DEFAULT_MAX_FRAME).push(encode(message, bytes, DEFAULT_MAX_FRAME))
  expect(frames).toHaveLength(1)
  const frame = frames[0]
  expect(frame.message).toEqual(message)
  expect(Array.from(frame.bytes ?? [])).toEqual([0, 1, 2, 255])
})

test("decodes the same frames at every split point", () => {
  const ping: Message = { type: "ping", seq: 7 }
  const chunk: Message = { type: "chunk", id: 3, value: { text: "hello" } }
  const stream = Buffer.concat([
    Buffer.from(encode(ping, undefined, DEFAULT_MAX_FRAME)),
    Buffer.from(encode(chunk, new Uint8Array([4, 5, 6]), DEFAULT_MAX_FRAME)),
  ])
  const whole = new Decoder(DEFAULT_MAX_FRAME)
  const expected = normalize(whole.push(stream))
  whole.end()
  expect(expected.length).toBe(2)

  for (let at = 0; at <= stream.length; at++) {
    const decoder = new Decoder(DEFAULT_MAX_FRAME)
    const decoded = [...decoder.push(stream.subarray(0, at)), ...decoder.push(stream.subarray(at))]
    decoder.end()
    expect(normalize(decoded)).toEqual(expected)
  }

  const oneByOne = new Decoder(DEFAULT_MAX_FRAME)
  const decoded: Frame[] = []
  for (const byte of stream) decoded.push(...oneByOne.push(new Uint8Array([byte])))
  oneByOne.end()
  expect(normalize(decoded)).toEqual(expected)
})

test("rejects a frame over its cap before its payload arrives", () => {
  expect(() => new Decoder(64).push(header(65, JSON))).toThrow("a frame of kind 0 and 65 bytes, over its cap")
})

test("rejects an unknown kind", () => {
  expect(() => new Decoder(DEFAULT_MAX_FRAME).push(header(0, 2))).toThrow("a frame of unknown kind 2")
})

test("rejects encoding over the cap", () => {
  expect(() => encode({ type: "ping", seq: 1 }, undefined, 2)).toThrow("a frame of 23 bytes, over its cap of 2")
})

test("rejects a malformed JSON payload", () => {
  const raw = Buffer.concat([header(1, JSON), Buffer.from("{")])
  expect(() => new Decoder(DEFAULT_MAX_FRAME).push(raw)).toThrow(/a frame whose JSON did not parse/)
})

test("reports a stream cut short", () => {
  const cutHeader = new Decoder(DEFAULT_MAX_FRAME)
  cutHeader.push(Buffer.from([1, 0, 0]))
  expect(() => cutHeader.end()).toThrow("a frame cut short: 3 of 5 bytes")

  const cutPayload = new Decoder(DEFAULT_MAX_FRAME)
  cutPayload.push(Buffer.concat([header(10, JSON), Buffer.alloc(4)]))
  expect(() => cutPayload.end()).toThrow("a frame cut short: 9 of 15 bytes")

  const clean = new Decoder(DEFAULT_MAX_FRAME)
  const first = encode({ type: "ping", seq: 7 }, undefined, DEFAULT_MAX_FRAME)
  const second = encode({ type: "pong", seq: 7, rss: 32 }, undefined, DEFAULT_MAX_FRAME)
  const frames = clean.push(Buffer.concat([Buffer.from(first), Buffer.from(second)]))
  expect(frames.map((frame) => frame.message)).toEqual([
    { type: "ping", seq: 7 },
    { type: "pong", seq: 7, rss: 32 },
  ])
  expect(() => clean.end()).not.toThrow()
})

test("decodes a large binary frame delivered in small chunks", () => {
  const bytes = Buffer.alloc(8 << 20)
  for (let at = 0; at < bytes.length; at++) bytes[at] = at & 0xff
  const message: Message = { type: "chunk", id: 1, value: { index: 0 } }
  const encoded = encode(message, bytes, DEFAULT_MAX_FRAME)
  const decoder = new Decoder(DEFAULT_MAX_FRAME)
  const frames: Frame[] = []
  const start = performance.now()
  for (let at = 0; at < encoded.length; at += 64 << 10) {
    frames.push(...decoder.push(encoded.subarray(at, at + (64 << 10))))
  }
  const elapsed = performance.now() - start
  decoder.end()
  expect(frames).toHaveLength(1)
  expect(frames[0].message).toEqual(message)
  const decoded = frames[0].bytes
  expect(decoded?.length).toBe(bytes.length)
  expect(decoded?.[0]).toBe(0)
  expect(decoded?.[100_000]).toBe(100_000 & 0xff)
  expect(decoded?.[bytes.length - 1]).toBe((bytes.length - 1) & 0xff)
  expect(elapsed).toBeLessThan(200)
})

test("reads maxFrame on every push", () => {
  const message: Message = { type: "ping", seq: 7 }
  const encoded = encode(message, undefined, DEFAULT_MAX_FRAME)
  const decoder = new Decoder(DEFAULT_MAX_FRAME)
  decoder.maxFrame = encoded.length - 5 - 1
  expect(() => decoder.push(encoded)).toThrow("over its cap")
  decoder.maxFrame = DEFAULT_MAX_FRAME
  expect(decoder.push(encoded)).toEqual([{ message }])

  const raised = new Decoder(0)
  raised.maxFrame = DEFAULT_MAX_FRAME
  expect(raised.push(encoded)).toEqual([{ message }])
})

test("queues bytes a full descriptor cannot take and flushes them in order without blocking", async () => {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), "sidecar-frame-"))
  const fifo = path.join(dir, "fifo")
  expect(Bun.spawnSync(["mkfifo", fifo]).exitCode).toBe(0)
  const fd = fs.openSync(fifo, fs.constants.O_RDWR | fs.constants.O_NONBLOCK)
  try {
    const payload = Buffer.alloc(1 << 20)
    for (let index = 0; index < payload.length; index++) payload[index] = index % 251
    const write = writer(fd)
    write(payload.subarray(0, 1 << 19))
    write(payload.subarray(1 << 19))
    const received: Buffer[] = []
    const chunk = Buffer.alloc(1 << 16)
    let size = 0
    while (size < payload.length) {
      const read = readAvailable(fd, chunk)
      if (read === 0) {
        await Bun.sleep(1)
        continue
      }
      received.push(Buffer.from(chunk.subarray(0, read)))
      size += read
    }
    expect(Buffer.concat(received).equals(payload)).toBe(true)
  } finally {
    fs.closeSync(fd)
    fs.rmSync(dir, { recursive: true, force: true })
  }
})

function readAvailable(fd: number, chunk: Buffer) {
  try {
    return fs.readSync(fd, chunk)
  } catch (error) {
    if (error instanceof Error && "code" in error && error.code === "EAGAIN") return 0
    throw error
  }
}
