import { defineScenario } from "../lib/scenario"

export default defineScenario({
  console: {
    "GET /console/api/policies/current-budgets": {
      status: 200,
      body: [
        {
          policyID: "policy_engineering",
          ruleID: "rule_astra",
          limitDollars: 20,
          spentDollars: 15,
          scope: "each",
          per: "model",
          modelCount: 1,
          coverage: "models",
          models: [
            {
              providerID: "opencode",
              connectionID: "conn_managed",
              modelID: "gpt-6-astra",
              name: "GPT-6 Astra",
              spentDollars: 15,
            },
          ],
          vendors: [{ id: "openai", name: "OpenAI" }],
          inputPriceAtLeast: 3,
          picked: false,
          shared: false,
          resetsAt: "2026-11-01T00:00:00.000Z",
        },
      ],
    },
    "GET /zen/go/v1/usage": {
      status: 200,
      body: {
        usage: {
          rolling: { status: "ok", percent: 42, resetsAt: "2026-10-24T17:00:00.000Z" },
          weekly: { status: "ok", percent: 18, resetsAt: "2026-10-24T17:00:00.000Z" },
          monthly: { status: "rate-limited", percent: 100, resetsAt: "2026-10-24T17:00:00.000Z" },
        },
      },
    },
  },
  steps: [
    { http: { method: "GET", path: "/api/experimental/usage/limits", as: "signedOut" } },
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
    { http: { method: "GET", path: "/api/experimental/usage/limits", as: "signedIn" } },
  ],
})
