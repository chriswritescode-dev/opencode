#!/usr/bin/env bun
const server = Bun.serve({
  hostname: "127.0.0.1",
  port: 0,
  fetch(request) {
    const url = new URL(request.url)
    if (request.method === "GET" && url.pathname === "/api/info")
      return Response.json({ version: process.env.FAKE_VERSION ?? "fake" })
    if (request.method === "GET" && url.pathname === "/api/event") return eventStream()
    return new Response(null, { status: 404 })
  },
})

function eventStream() {
  const encoder = new TextEncoder()
  return new Response(
    new ReadableStream<Uint8Array>({
      start(controller) {
        controller.enqueue(encoder.encode('data: {"id":"evt_fixture","type":"server.connected","data":{}}\n\n'))
      },
    }),
    { headers: { "content-type": "text/event-stream" } },
  )
}

console.log(JSON.stringify({ url: `http://127.0.0.1:${server.port}` }))
await Bun.stdin.text()
server.stop(true)
process.exit(0)
