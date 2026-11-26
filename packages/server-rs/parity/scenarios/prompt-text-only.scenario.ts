import { defineScenario } from "../lib/scenario"

export default defineScenario({
  config: {
    model: "anthropic-test/test-model",
    agents: { build: { permissions: [{ action: "*", resource: "*", effect: "deny" }] } },
  },
  model: [
    {
      cassette:
        "packages/ai/test/fixtures/recordings/anthropic-compatible-messages/minimax-m3-anthropic-compatible-text.json",
    },
  ],
  steps: [
    { http: { method: "POST", path: "/api/session", body: {}, as: "s" } },
    { http: { method: "POST", path: "/api/session/{{s.data.id}}/prompt", body: { text: "say hi" } } },
    { waitEvent: { type: "session.execution.succeeded" } },
    { http: { method: "POST", path: "/api/experimental/session/{{s.data.id}}/wait" } },
    { http: { method: "GET", path: "/api/session/{{s.data.id}}/message" } },
  ],
})
