import { defineScenario } from "../lib/scenario"

export default defineScenario({
  config: {
    plugins: ["-opencode.browser", "-opencode.goal", "-opencode.home-memory"],
    automations: false,
  },
  steps: [
    { http: { method: "GET", path: "/api/permission/request" } },
    { http: { method: "GET", path: "/api/permission/saved" } },
    {
      http: {
        method: "PATCH",
        path: "/api/permission/saved/psv_unknown",
        body: { action: "bash", resource: "ls *" },
      },
    },
    { http: { method: "DELETE", path: "/api/permission/saved/psv_unknown" } },
  ],
})
