import { Connection } from "./connection"
import { Decoder, DEFAULT_MAX_FRAME, write, type Frame } from "./frame"
import { register } from "./methods"
import { MAGIC, VERSION } from "./protocol.gen"
import { killAll, supervise } from "./supervise"

supervise()

const decoder = new Decoder(DEFAULT_MAX_FRAME)
const frames = readFrames(decoder)
const hello = await readHello(frames[Symbol.asyncIterator]())
if (hello.version !== VERSION) process.exit(3)
decoder.maxFrame = hello.maxFrame

const connection = new Connection(
  { write: (bytes) => write(bytes, 0), frames },
  { window: hello.window, maxFrame: hello.maxFrame },
)

register(connection, { test: hello.test, maxFrame: hello.maxFrame, write: (bytes) => write(bytes, 0) })

connection.onControl = (message) => {
  if (message.type !== "ping") return
  connection.send({ type: "pong", seq: message.seq, rss: process.memoryUsage().rss })
}

process.on("unhandledRejection", (reason) => {
  const detail = reason instanceof Error ? reason.message : String(reason)
  try {
    connection.send({ type: "log", level: "error", message: detail.replace(/\s+/g, " ").trim() })
  } catch {}
})

connection.send({ type: "ready", magic: MAGIC, version: VERSION, pid: process.pid })

await connection.done
killAll(process.pid)

async function* readFrames(decoder: Decoder): AsyncIterable<Frame> {
  for await (const chunk of Bun.stdin.stream()) {
    for (const frame of decoder.push(chunk)) yield frame
  }
  decoder.end()
}

async function readHello(iterator: AsyncIterator<Frame>) {
  let next = await iterator.next()
  while (!next.done) {
    if (next.value.message.type === "hello") return next.value.message
    next = await iterator.next()
  }
  return process.exit(0)
}
