import { defineScenario } from "../lib/scenario"

export default defineScenario({
  steps: [
    { http: { method: "GET", path: "/api/info", auth: "basic" } },
    { http: { method: "GET", path: "/api/info", auth: "query" } },
    { http: { method: "GET", path: "/api/info", auth: "none" } },
  ],
})
