import { defineScenario } from "../lib/scenario"

export default defineScenario({
  files: { "plain.txt": "plain\n" },
  steps: [
    { http: { method: "GET", path: "/api/vcs", as: "info" } },
    { http: { method: "GET", path: "/api/vcs/status", as: "status" } },
    { http: { method: "POST", path: "/api/vcs/init", query: { provider: "nope" }, as: "unknownProvider" } },
    { http: { method: "POST", path: "/api/vcs/init", as: "initialized" } },
    { http: { method: "POST", path: "/api/vcs/init", as: "conflict" } },
  ],
})
