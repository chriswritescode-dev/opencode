import { defineScenario } from "../lib/scenario"

const models = {
  "parity-fixture": {
    id: "parity-fixture",
    name: "Parity Fixture",
    env: ["PARITY_FIXTURE_API_KEY"],
    models: {
      "fixture-chat": {
        id: "fixture-chat",
        type: "chat",
        name: "Fixture Chat",
        release_date: "2026-01-15",
        modalities: { input: ["text"], output: ["text"] },
        capabilities: { tools: { supported: true }, reasoning: { supported: false } },
        limit: { context: 64000, output: 4096 },
        api: {
          "chat-completions": {
            base_url: "https://parity-fixture.test/v1",
            sdk: { aisdk: "@ai-sdk/openai-compatible" },
          },
        },
      },
    },
  },
}

export default defineScenario({
  models,
  steps: [
    { http: { method: "GET", path: "/api/provider" } },
    { http: { method: "GET", path: "/api/model" } },
    {
      http: {
        method: "POST",
        path: "/api/integration/parity-fixture/connect/key",
        body: { key: "fixture-key" },
      },
    },
    { http: { method: "GET", path: "/api/provider" } },
    { http: { method: "GET", path: "/api/model" } },
  ],
})
