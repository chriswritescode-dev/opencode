import path from "node:path"
import type { Reply } from "./scenario"

export type ModelRequest = { method: string; path: string; body: unknown }

export type ModelEndpoint = {
  url: string
  load(replies: Reply[], template: { work: string; root: string }): void
  requests(): ModelRequest[]
  unused(): number
  failures(): string[]
  stop(): Promise<void>
}

type Template = { work: string; root: string }
type State = { replies: Reply[]; consumed: number; log: ModelRequest[]; failures: string[]; template: Template }

const repository = path.resolve(import.meta.dir, "../../../..")
const placeholder = /\{\{(work|root)\}\}/g
const routes = new Set(["/v1/chat/completions", "/v1/messages"])

export async function startModelEndpoint(): Promise<ModelEndpoint> {
  const state: State = { replies: [], consumed: 0, log: [], failures: [], template: { work: "", root: "" } }
  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    fetch: (request) => respond(request, state),
  })
  return {
    url: `http://127.0.0.1:${server.port}`,
    load: (replies, template) => {
      state.replies = [...replies]
      state.consumed = 0
      state.log = []
      state.failures = []
      state.template = template
    },
    requests: () => [...state.log],
    unused: () => state.replies.length - state.consumed,
    failures: () => [...state.failures],
    stop: async () => {
      await server.stop(true)
    },
  }
}

async function respond(request: Request, state: State): Promise<Response> {
  const pathname = new URL(request.url).pathname
  const accepted = request.method === "POST" && routes.has(pathname)
  const reply = accepted ? state.replies[state.consumed] : undefined
  if (reply !== undefined) state.consumed += 1
  const body = await readBody(request)
  state.log.push({ method: request.method, path: pathname, body })
  if (!accepted) return error(404, "not found")
  if (reply === undefined) return failure(state, "unscripted model request")
  return encode(reply, pathname, body, state)
}

async function encode(reply: Reply, pathname: string, body: unknown, state: State) {
  if ("cassette" in reply) return replay(reply, state)
  const anthropic = pathname === "/v1/messages"
  if ("tool" in reply) {
    const tool = { id: reply.tool.id, name: reply.tool.name, input: templateValue(reply.tool.input, state.template) }
    return stream(anthropic ? anthropicTool(tool, requestModel(body)) : openaiTool(tool))
  }
  const text = templateText(reply.text, state.template)
  return stream(anthropic ? anthropicText(text, requestModel(body)) : openaiText(text))
}

function openaiTool(tool: { id: string; name: string; input: unknown }) {
  return openaiFrames([
    {
      choices: [
        {
          delta: {
            role: "assistant",
            tool_calls: [{ index: 0, id: tool.id, type: "function", function: { name: tool.name, arguments: "" } }],
          },
          finish_reason: null,
        },
      ],
      usage: null,
    },
    {
      choices: [{ delta: { tool_calls: [{ index: 0, function: { arguments: JSON.stringify(tool.input) } }] } }],
      usage: null,
    },
    { choices: [{ delta: {}, finish_reason: "tool_calls" }], usage: null },
    { choices: [], usage: { prompt_tokens: 10, completion_tokens: 1, total_tokens: 11 } },
  ])
}

function openaiText(text: string) {
  return openaiFrames([
    { choices: [{ delta: { role: "assistant" }, finish_reason: null }], usage: null },
    { choices: [{ delta: { content: text }, finish_reason: null }], usage: null },
    { choices: [{ delta: {}, finish_reason: "stop" }], usage: null },
    { choices: [], usage: { prompt_tokens: 10, completion_tokens: 1, total_tokens: 11 } },
  ])
}

function openaiFrames(chunks: unknown[]) {
  return `${chunks.map((chunk) => `data: ${JSON.stringify(chunk)}\n\n`).join("")}data: [DONE]\n\n`
}

function anthropicTool(tool: { id: string; name: string; input: unknown }, model: string) {
  return anthropicFrames([
    ["message_start", messageStart(model)],
    [
      "content_block_start",
      {
        type: "content_block_start",
        index: 0,
        content_block: { type: "tool_use", id: tool.id, name: tool.name, input: {} },
      },
    ],
    [
      "content_block_delta",
      {
        type: "content_block_delta",
        index: 0,
        delta: { type: "input_json_delta", partial_json: JSON.stringify(tool.input) },
      },
    ],
    ["content_block_stop", { type: "content_block_stop", index: 0 }],
    [
      "message_delta",
      {
        type: "message_delta",
        delta: { stop_reason: "tool_use", stop_sequence: null },
        usage: { output_tokens: 1 },
      },
    ],
    ["message_stop", { type: "message_stop" }],
  ])
}

function anthropicText(text: string, model: string) {
  return anthropicFrames([
    ["message_start", messageStart(model)],
    ["content_block_start", { type: "content_block_start", index: 0, content_block: { type: "text", text: "" } }],
    ["content_block_delta", { type: "content_block_delta", index: 0, delta: { type: "text_delta", text } }],
    ["content_block_stop", { type: "content_block_stop", index: 0 }],
    [
      "message_delta",
      {
        type: "message_delta",
        delta: { stop_reason: "end_turn", stop_sequence: null },
        usage: { output_tokens: 1 },
      },
    ],
    ["message_stop", { type: "message_stop" }],
  ])
}

function messageStart(model: string) {
  return {
    type: "message_start",
    message: {
      id: "msg_parity",
      type: "message",
      role: "assistant",
      content: [],
      model,
      stop_reason: null,
      stop_sequence: null,
      usage: { input_tokens: 10, output_tokens: 1 },
    },
  }
}

function anthropicFrames(frames: [string, unknown][]) {
  return frames.map(([event, data]) => `event: ${event}\ndata: ${JSON.stringify(data)}\n\n`).join("")
}

function requestModel(body: unknown) {
  return isRecord(body) && typeof body.model === "string" ? body.model : "parity-test"
}

function stream(body: string) {
  return new Response(body, { headers: { "content-type": "text/event-stream" } })
}

function error(status: number, reason: string) {
  return Response.json({ error: reason }, { status })
}

async function readBody(request: Request): Promise<unknown> {
  const text = await request.text()
  if (text === "") return undefined
  const parsed = parseJSON(text)
  return parsed.ok ? parsed.value : text
}

async function replay(reply: { cassette: string; interaction?: number }, state: State): Promise<Response> {
  const file = path.join(repository, reply.cassette)
  const loaded = await readCassette(file)
  if (!loaded.ok) return failure(state, loaded.reason)
  const index = reply.interaction ?? 0
  const interactions =
    isRecord(loaded.value) && Array.isArray(loaded.value.interactions)
      ? loaded.value.interactions.filter(isHttpInteraction)
      : []
  const interaction = interactions[index]
  if (!isRecord(interaction) || !isRecord(interaction.response))
    return failure(state, `cassette ${reply.cassette} has no interaction ${index}`)
  const response = interaction.response
  const recorded = typeof response.body === "string" ? response.body : ""
  return new Response(response.bodyEncoding === "base64" ? Buffer.from(recorded, "base64") : recorded, {
    status: typeof response.status === "number" ? response.status : 200,
    headers: stringHeaders(response.headers),
  })
}

function failure(state: State, reason: string) {
  state.failures.push(reason)
  return error(500, reason)
}

function isHttpInteraction(value: unknown) {
  return isRecord(value) && value.transport === "http"
}

async function readCassette(file: string): Promise<{ ok: true; value: unknown } | { ok: false; reason: string }> {
  try {
    return { ok: true, value: await Bun.file(file).json() }
  } catch (cause) {
    const reason = message(cause).replace(/\s+/g, " ")
    return { ok: false, reason: `cannot read cassette ${path.relative(repository, file)}: ${reason}` }
  }
}

function stringHeaders(value: unknown) {
  if (!isRecord(value)) return {}
  return Object.fromEntries(
    Object.entries(value).flatMap(([name, header]) => (typeof header === "string" ? [[name, header]] : [])),
  )
}

function templateText(text: string, template: Template) {
  return text.replace(placeholder, (_, name: "work" | "root") => template[name])
}

function templateValue(value: unknown, template: Template): unknown {
  if (typeof value === "string") return templateText(value, template)
  if (Array.isArray(value)) return value.map((entry) => templateValue(entry, template))
  if (isRecord(value))
    return Object.fromEntries(Object.entries(value).map(([key, entry]) => [key, templateValue(entry, template)]))
  return value
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value)
}

function parseJSON(text: string): { ok: true; value: unknown } | { ok: false } {
  try {
    return { ok: true, value: JSON.parse(text) }
  } catch {
    return { ok: false }
  }
}

function message(cause: unknown) {
  return cause instanceof Error ? cause.message : String(cause)
}
