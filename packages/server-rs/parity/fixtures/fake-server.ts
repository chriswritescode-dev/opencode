#!/usr/bin/env bun
const encoder = new TextEncoder()
const subscribers = new Set<ReadableStreamDefaultController<Uint8Array>>()

const server = Bun.serve({
  hostname: "127.0.0.1",
  port: 0,
  fetch(request, server) {
    const url = new URL(request.url)
    if (request.method === "GET" && url.pathname === "/echo") {
      if (server.upgrade(request)) return
      return new Response(null, { status: 400 })
    }
    if (request.method === "GET" && url.pathname === "/api/info")
      return Response.json({ version: process.env.FAKE_VERSION ?? "fake" })
    if (request.method === "GET" && url.pathname === "/api/event") return eventStream()
    if (request.method === "GET" && url.pathname === "/api/config") {
      scheduleWarmEvent()
      return Response.json({})
    }
    return new Response(null, { status: 404 })
  },
  websocket: {
    message(socket, message) {
      socket.send(message)
    },
  },
})

function eventStream() {
  let sink: ReadableStreamDefaultController<Uint8Array> | undefined
  return new Response(
    new ReadableStream<Uint8Array>({
      start(controller) {
        sink = controller
        subscribers.add(controller)
        controller.enqueue(encoder.encode('data: {"id":"evt_fixture","type":"server.connected","data":{}}\n\n'))
      },
      cancel() {
        if (sink !== undefined) subscribers.delete(sink)
      },
    }),
    { headers: { "content-type": "text/event-stream" } },
  )
}

function scheduleWarmEvent() {
  const delay = Number(process.env.FAKE_WARM_DELAY ?? 0)
  setTimeout(() => {
    const frame = encoder.encode('data: {"type":"plugin.updated","data":{"id":"fixture"}}\n\n')
    for (const controller of subscribers) {
      try {
        controller.enqueue(frame)
      } catch {
        subscribers.delete(controller)
      }
    }
  }, delay)
}

console.log(JSON.stringify({ url: `http://127.0.0.1:${server.port}` }))
await Bun.stdin.text()
server.stop(true)
process.exit(0)
