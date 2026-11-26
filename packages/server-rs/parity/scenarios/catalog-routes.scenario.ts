import { defineScenario } from "../lib/scenario"

export default defineScenario({
  steps: [
    { http: { method: "GET", path: "/api/provider", as: "providers" } },
    { http: { method: "GET", path: "/api/provider/openai-test", as: "provider" } },
    { http: { method: "GET", path: "/api/provider/unknown-provider" } },
    { http: { method: "GET", path: "/api/model", as: "models" } },
    { http: { method: "GET", path: "/api/model", query: { types: "chat" } } },
    { http: { method: "GET", path: "/api/model", query: { types: "image" } } },
    { http: { method: "GET", path: "/api/model/default", as: "defaultModel" } },
  ],
})
