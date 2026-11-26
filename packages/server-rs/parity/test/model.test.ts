import { expect, test } from "bun:test"
import fs from "node:fs/promises"
import path from "node:path"
import { startModelEndpoint, type ModelEndpoint } from "../lib/model"

const repository = path.resolve(import.meta.dir, "../../../..")
const template = { work: "/tmp/opencode-parity/work", root: "/tmp/opencode-parity" }
const cassette =
  "packages/ai/test/fixtures/recordings/anthropic-compatible-messages/minimax-m3-anthropic-compatible-text.json"

type OpenAIChunk = {
  choices: Array<{
    delta: { content?: string; tool_calls?: Array<{ id?: string; function?: { name?: string; arguments?: string } }> }
    finish_reason: string | null
  }>
  usage: { total_tokens: number } | null
}

type AnthropicEvent = {
  event: string
  data: {
    type?: string
    delta?: { type?: string; text?: string; stop_reason?: string; partial_json?: string }
    content_block?: { type?: string; id?: string; name?: string }
  }
}

async function withModel(run: (model: ModelEndpoint) => Promise<void>) {
  const model = await startModelEndpoint()
  try {
    await run(model)
  } finally {
    await model.stop()
  }
}

function post(model: ModelEndpoint, pathname: string, body: unknown) {
  return fetch(new URL(pathname, model.url), {
    method: "POST",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(body),
  })
}

function openaiChunks(text: string) {
  return text
    .split("\n\n")
    .flatMap((block) =>
      block
        .split("\n")
        .filter((line) => line.startsWith("data: "))
        .map((line) => line.slice(6)),
    )
    .filter((data) => data !== "[DONE]")
    .map((data) => JSON.parse(data) as OpenAIChunk)
}

function anthropicEvents(text: string) {
  return text
    .split("\n\n")
    .filter((block) => block.trim() !== "")
    .map((block) => {
      const lines = block.split("\n")
      const event = lines.filter((line) => line.startsWith("event: ")).map((line) => line.slice(7))[0] ?? ""
      const data = lines
        .filter((line) => line.startsWith("data: "))
        .map((line) => line.slice(6))
        .join("\n")
      return { event, data: JSON.parse(data) as AnthropicEvent["data"] }
    })
}

function toolCalls(chunks: OpenAIChunk[]) {
  return chunks.flatMap((chunk) => chunk.choices.flatMap((choice) => choice.delta.tool_calls ?? []))
}

test("streams an OpenAI text reply with a stop chunk and usage", async () => {
  await withModel(async (model) => {
    model.load([{ text: "Hello!" }], template)
    const response = await post(model, "/v1/chat/completions", { model: "test-model", messages: [] })
    expect(response.status).toBe(200)
    expect(response.headers.get("content-type")).toBe("text/event-stream")
    const chunks = openaiChunks(await response.text())
    expect(chunks.map((chunk) => chunk.choices[0]?.finish_reason).filter((reason) => reason === "stop")).toHaveLength(1)
    expect(
      chunks
        .flatMap((chunk) => chunk.choices.map((choice) => choice.delta.content))
        .filter((content) => content !== undefined),
    ).toEqual(["Hello!"])
    expect(chunks.at(-1)?.usage?.total_tokens).toBe(11)
    expect(model.requests()).toEqual([
      { method: "POST", path: "/v1/chat/completions", body: { model: "test-model", messages: [] } },
    ])
    expect(model.unused()).toBe(0)
  })
})

test("streams an OpenAI tool reply with accumulated arguments", async () => {
  await withModel(async (model) => {
    model.load([{ tool: { id: "call_1", name: "read", input: { path: "a.txt" } } }], template)
    const chunks = openaiChunks(await (await post(model, "/v1/chat/completions", { model: "test-model" })).text())
    expect(
      chunks.map((chunk) => chunk.choices[0]?.finish_reason).filter((reason) => reason === "tool_calls"),
    ).toHaveLength(1)
    const calls = toolCalls(chunks)
    expect(calls[0]?.id).toBe("call_1")
    expect(calls[0]?.function?.name).toBe("read")
    const argumentsText = calls.map((call) => call.function?.arguments ?? "").join("")
    expect(JSON.parse(argumentsText)).toEqual({ path: "a.txt" })
  })
})

test("streams an Anthropic text reply", async () => {
  await withModel(async (model) => {
    model.load([{ text: "Hello!" }], template)
    const response = await post(model, "/v1/messages", { model: "claude-parity" })
    expect(response.headers.get("content-type")).toBe("text/event-stream")
    const events = anthropicEvents(await response.text())
    expect(events[0]?.event).toBe("message_start")
    expect(
      events.filter((event) => event.data.delta?.type === "text_delta").map((event) => event.data.delta?.text),
    ).toEqual(["Hello!"])
    const messageDelta = events.filter((event) => event.event === "message_delta")
    expect(messageDelta.map((event) => event.data.delta?.stop_reason)).toEqual(["end_turn"])
  })
})

test("streams an Anthropic tool reply", async () => {
  await withModel(async (model) => {
    model.load([{ tool: { id: "toolu_1", name: "read", input: { path: "a.txt" } } }], template)
    const events = anthropicEvents(await (await post(model, "/v1/messages", { model: "claude-parity" })).text())
    const blocks = events.flatMap((event) => (event.data.content_block === undefined ? [] : [event.data.content_block]))
    expect(blocks[0]?.type).toBe("tool_use")
    expect(blocks[0]?.id).toBe("toolu_1")
    expect(blocks[0]?.name).toBe("read")
    const partial = events.filter((event) => event.data.delta?.partial_json !== undefined)
    expect(partial.map((event) => JSON.parse(event.data.delta?.partial_json ?? ""))).toEqual([{ path: "a.txt" }])
    const messageDelta = events.filter((event) => event.event === "message_delta")
    expect(messageDelta.map((event) => event.data.delta?.stop_reason)).toEqual(["tool_use"])
  })
})

test("replays a recorded cassette interaction verbatim", async () => {
  await withModel(async (model) => {
    const recorded = (await Bun.file(path.join(repository, cassette)).json()) as {
      interactions: Array<{ response: { status: number; headers: Record<string, string>; body: string } }>
    }
    const interaction = recorded.interactions[0]
    model.load([{ cassette }], template)
    const response = await post(model, "/v1/messages", { model: "MiniMax-M3" })
    expect(response.status).toBe(interaction?.response.status)
    expect(response.headers.get("content-type")).toBe(interaction?.response.headers["content-type"])
    expect(await response.text()).toBe(interaction?.response.body)
  })
})

test("rejects an exhausted script and logs unknown routes", async () => {
  await withModel(async (model) => {
    model.load([{ text: "Hello!" }], template)
    expect((await post(model, "/v1/chat/completions", { model: "test-model" })).status).toBe(200)
    const exhausted = await post(model, "/v1/chat/completions", { model: "test-model" })
    expect(exhausted.status).toBe(500)
    expect(await exhausted.json()).toEqual({ error: "unscripted model request" })
    expect(model.failures()).toEqual(["unscripted model request"])
    expect(model.unused()).toBe(0)
    const missing = await post(model, "/v1/unknown", { model: "test-model" })
    expect(missing.status).toBe(404)
    expect(await missing.json()).toEqual({ error: "not found" })
    const requests = model.requests()
    expect(requests.map((request) => request.path)).toEqual([
      "/v1/chat/completions",
      "/v1/chat/completions",
      "/v1/unknown",
    ])
    expect(model.unused()).toBe(0)
  })
})

test("records cassette failures and clears them on load", async () => {
  await withModel(async (model) => {
    const missing = `packages/server-rs/parity/out/missing-${crypto.randomUUID()}.json`
    model.load([{ cassette: missing }], template)
    const missingResponse = await post(model, "/v1/messages", { model: "claude-parity" })
    expect(missingResponse.status).toBe(500)
    expect(model.unused()).toBe(0)
    const missingFailures = model.failures()
    expect(missingFailures).toHaveLength(1)
    expect(missingFailures[0]).toContain("cannot read cassette")
    expect(missingFailures[0]).toContain(missing)
    model.load([{ cassette, interaction: 99 }], template)
    expect(model.failures()).toEqual([])
    const outOfRange = await post(model, "/v1/messages", { model: "MiniMax-M3" })
    expect(outOfRange.status).toBe(500)
    expect(model.unused()).toBe(0)
    expect(model.failures()).toEqual([`cassette ${cassette} has no interaction 99`])
    model.load([{ text: "Hello!" }], template)
    expect(model.failures()).toEqual([])
  })
})

test("skips non-http cassette interactions when indexing", async () => {
  const file = path.join(repository, "packages/server-rs/parity/out", `model-test-${crypto.randomUUID()}.json`)
  const recorded = {
    status: 201,
    headers: { "content-type": "application/json" },
    body: '{"ok":true}',
  }
  await Bun.write(
    file,
    JSON.stringify({
      version: 1,
      interactions: [
        { transport: "websocket", messages: [] },
        {
          transport: "http",
          request: { method: "POST", url: "https://example.test/v1/messages", headers: {}, body: "" },
          response: recorded,
        },
      ],
    }),
  )
  try {
    await withModel(async (model) => {
      model.load([{ cassette: path.relative(repository, file) }], template)
      const response = await post(model, "/v1/messages", { model: "claude-parity" })
      expect(response.status).toBe(recorded.status)
      expect(response.headers.get("content-type")).toBe(recorded.headers["content-type"])
      expect(await response.text()).toBe(recorded.body)
      expect(model.failures()).toEqual([])
      expect(model.unused()).toBe(0)
    })
  } finally {
    await fs.rm(file, { force: true })
  }
})

test("answers scripted console routes and records their requests separately", async () => {
  await withModel(async (model) => {
    model.load([], template, {
      "GET /console/api/policies/current-budgets": { status: 200, body: [{ ruleID: "rule_astra" }] },
      "GET /zen/go/v1/usage": { status: 201, body: { usage: { rolling: null } } },
      "GET /zen/empty": { status: 204 },
    })
    const budgets = await fetch(new URL("/console/api/policies/current-budgets", model.url), {
      headers: { authorization: "Bearer console-access", "x-org-id": "wrk_anomaly" },
    })
    expect(budgets.status).toBe(200)
    expect(await budgets.json()).toEqual([{ ruleID: "rule_astra" }])
    const go = await fetch(new URL("/zen/go/v1/usage", model.url), {
      headers: { authorization: "Bearer console-access", "x-opencode-org-id": "wrk_anomaly" },
    })
    expect(go.status).toBe(201)
    expect(await go.json()).toEqual({ usage: { rolling: null } })
    const empty = await fetch(new URL("/zen/empty", model.url))
    expect(empty.status).toBe(204)
    expect(model.requests()).toEqual([])
    expect(model.console()).toEqual([
      {
        method: "GET",
        path: "/console/api/policies/current-budgets",
        authorization: "Bearer console-access",
        xOrgID: "wrk_anomaly",
        xOpencodeOrgID: null,
      },
      {
        method: "GET",
        path: "/zen/empty",
        authorization: null,
        xOrgID: null,
        xOpencodeOrgID: null,
      },
      {
        method: "GET",
        path: "/zen/go/v1/usage",
        authorization: "Bearer console-access",
        xOrgID: null,
        xOpencodeOrgID: "wrk_anomaly",
      },
    ])
    expect(model.unused()).toBe(0)
  })
})

test("holds barrier console responses until every barrier route is requested", async () => {
  await withModel(async (model) => {
    model.load([], template, {
      "GET /console/one": { status: 200, body: "one", barrier: true },
      "GET /console/two": { status: 200, body: "two", barrier: true },
      "GET /console/sequential": { status: 200, body: "sequential" },
    })
    const sequential = await fetch(new URL("/console/sequential", model.url))
    expect(await sequential.text()).toBe("sequential")

    const first = fetch(new URL("/console/one", model.url))
    await until(() => model.console().length === 1)
    expect(await Promise.race([first.then(() => "settled"), Bun.sleep(25).then(() => "waiting")])).toBe("waiting")

    const second = fetch(new URL("/console/two", model.url))
    expect(await (await first).text()).toBe("one")
    expect(await (await second).text()).toBe("two")
  })
})

async function until(check: () => boolean) {
  for (let attempt = 0; attempt < 1_000; attempt += 1) {
    if (check()) return
    await Bun.sleep(1)
  }
  throw new Error("condition not met")
}

test("ignores undeclared console requests and clears scripted routes on load", async () => {
  await withModel(async (model) => {
    model.load([], template, { "GET /console/api/policies/current-budgets": { status: 200, body: [] } })
    expect((await fetch(new URL("/console/api/v2/config", model.url))).status).toBe(404)
    expect((await fetch(new URL("/zen/go/v1/other", model.url))).status).toBe(404)
    expect(model.console()).toEqual([])
    model.load([], template)
    expect((await fetch(new URL("/console/api/policies/current-budgets", model.url))).status).toBe(404)
    expect(model.console()).toEqual([])
    expect(model.requests()).toEqual([])
    expect(model.failures()).toEqual([])
  })
})

test("templates {{work}} and {{root}} through nested tool input", async () => {
  await withModel(async (model) => {
    model.load(
      [
        {
          tool: {
            id: "call_1",
            name: "read",
            input: {
              path: "{{work}}/file.txt",
              nested: [{ note: "{{root}} and {{other}}" }],
              count: 2,
            },
          },
        },
      ],
      template,
    )
    const chunks = openaiChunks(await (await post(model, "/v1/chat/completions", { model: "test-model" })).text())
    const argumentsText = toolCalls(chunks)
      .map((call) => call.function?.arguments ?? "")
      .join("")
    expect(JSON.parse(argumentsText)).toEqual({
      path: `${template.work}/file.txt`,
      nested: [{ note: `${template.root} and {{other}}` }],
      count: 2,
    })
  })
})
