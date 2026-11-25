import { defineScenario } from "../lib/scenario"

export default defineScenario({
  steps: [
    { http: { method: "GET", path: "/api/agent" } },
    { http: { method: "GET", path: "/api/model" } },
  ],
})
