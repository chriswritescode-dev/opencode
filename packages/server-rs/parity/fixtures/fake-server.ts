#!/usr/bin/env bun
const server = Bun.serve({
  hostname: "127.0.0.1",
  port: 0,
  fetch(request) {
    const url = new URL(request.url)
    if (request.method === "GET" && url.pathname === "/api/info")
      return Response.json({ version: process.env.FAKE_VERSION ?? "fake" })
    return new Response(null, { status: 404 })
  },
})

console.log(JSON.stringify({ url: `http://127.0.0.1:${server.port}` }))
await Bun.stdin.text()
server.stop(true)
process.exit(0)
