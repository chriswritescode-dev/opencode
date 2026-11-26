import { defineScenario } from "../lib/scenario"

export default defineScenario({
  steps: [
    { http: { method: "GET", path: "/api/plugin" } },
    { http: { method: "GET", path: "/api/plugin/gui", query: { id: "missing" } } },
    { http: { method: "POST", path: "/api/plugin/check", body: {} } },
    { http: { method: "POST", path: "/api/plugin/check", body: { target: "nope" } } },
    { http: { method: "POST", path: "/api/plugin/update", body: { targets: ["nope"] } } },
  ],
})
