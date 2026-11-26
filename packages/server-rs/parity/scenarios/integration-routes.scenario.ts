import { defineScenario } from "../lib/scenario"

export default defineScenario({
  steps: [
    { http: { method: "GET", path: "/api/integration/openai-test", as: "integration" } },
    { http: { method: "GET", path: "/api/integration/unknown-integration" } },
    {
      http: {
        method: "POST",
        path: "/api/integration/openai-test/connect/key",
        body: { key: "parity-key" },
      },
    },
    {
      http: {
        method: "POST",
        path: "/api/integration/openai-test/connect/external",
        body: { methodID: "unknown-method" },
      },
    },
    { http: { method: "GET", path: "/api/integration/openai-test/connect/oauth/unknown-attempt" } },
    { http: { method: "GET", path: "/api/integration/openai-test/connect/command/unknown-attempt" } },
    {
      http: {
        method: "POST",
        path: "/api/experimental/integration/wellknown",
        body: { url: "http://127.0.0.1:1" },
      },
    },
  ],
})
