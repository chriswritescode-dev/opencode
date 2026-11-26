import { defineScenario } from "../lib/scenario"

export default defineScenario({
  console: {
    "GET /console/api/policies/current-budgets": { status: 502, body: "upstream down", barrier: true },
    "GET /zen/go/v1/usage": { status: 403, body: { type: "error" }, barrier: true },
  },
  steps: [
    {
      http: {
        method: "POST",
        path: "/api/credential",
        body: {
          integrationID: "opencode",
          label: "Anomaly",
          value: {
            type: "oauth",
            methodID: "device",
            access: "console-access",
            refresh: "console-refresh",
            expires: 0,
            metadata: { server: "{{endpoint}}/console", orgID: "wrk_anomaly", orgName: "Anomaly" },
          },
        },
        as: "credential",
      },
    },
    { http: { method: "GET", path: "/api/experimental/usage/limits" } },
  ],
})
