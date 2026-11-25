import { expect, test } from "bun:test"
import path from "node:path"
import { coverageReport, loadSurface, matchOperation, type CoverageItem } from "../lib/coverage"

const repository = path.resolve(import.meta.dir, "../../../..")
const surface = await loadSurface(repository)
const scenariosOf = (items: CoverageItem[], id: string) => items.find((item) => item.id === id)?.scenarios

test("loads the pinned OpenAPI surface", () => {
  expect(surface.operations).toHaveLength(188)
  expect(surface.websockets).toHaveLength(4)
  expect(surface.websockets.map((operation) => operation.id).sort()).toEqual([
    "experimental.machine.link",
    "experimental.mcp.client",
    "experimental.persistentPty.connect",
    "pty.connect",
  ])
  expect(surface.events).toHaveLength(102)
  expect(surface.events).toContain("server.connected")
  expect(surface.events).toContain("rpc.*")
})

test("matches templated paths to the most specific operation", () => {
  expect(matchOperation(surface, "GET", "/api/fs/read/hello.txt")?.path).toBe("/api/fs/read/*")
  expect(matchOperation(surface, "GET", "/api/session/ses_x/message")?.path).toBe("/api/session/{sessionID}/message")
})

test("reports coverage for operations, websockets and events", () => {
  const coverage = coverageReport(surface, [
    {
      name: "demo",
      requests: [
        { protocol: "http", method: "GET", path: "/api/info" },
        { protocol: "http", method: "GET", path: "/openapi.json" },
        { protocol: "ws", method: "GET", path: "/api/pty/pty_1/connect" },
      ],
      events: [{ type: "server.connected" }, { type: "rpc.custom" }],
    },
  ])
  expect(scenariosOf(coverage.operations, "server.info")).toEqual(["demo"])
  expect(scenariosOf(coverage.websockets, "pty.connect")).toEqual(["demo"])
  expect(scenariosOf(coverage.events, "server.connected")).toEqual(["demo"])
  expect(scenariosOf(coverage.events, "rpc.*")).toEqual(["demo"])
  expect(scenariosOf(coverage.operations, "session.create")).toEqual([])
  expect(coverage.operations.find((item) => item.id === "session.create")?.covered).toBe(false)
  expect(coverage.unmatched).toEqual(["GET /openapi.json"])
})
