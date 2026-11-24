import { defineScenario } from "../lib/scenario"

export default defineScenario({
  files: { "hello.txt": "hello parity\n" },
  model: [{ tool: { id: "call_read", name: "read", input: { path: "{{work}}/hello.txt" } } }, { text: "done" }],
  steps: [
    { http: { method: "POST", path: "/api/session", body: {}, as: "s" } },
    { http: { method: "POST", path: "/api/session/{{s.data.id}}/prompt", body: { text: "read hello.txt" } } },
    { waitEvent: { type: "session.execution.succeeded" } },
    { http: { method: "POST", path: "/api/experimental/session/{{s.data.id}}/wait" } },
    { http: { method: "GET", path: "/api/session/{{s.data.id}}/message" } },
  ],
})
