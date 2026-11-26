import { defineScenario } from "../lib/scenario"

const integrations = [
  "amazon-bedrock",
  "azure",
  "cloudflare-ai-gateway",
  "cloudflare-workers-ai",
  "databricks",
  "digitalocean",
  "github-copilot",
  "gitlab",
  "google-vertex",
  "neon",
  "openai",
  "poe",
  "snowflake-cortex",
  "xai",
]

export default defineScenario({
  steps: [
    ...integrations.map((id) => ({ http: { method: "GET" as const, path: `/api/integration/${id}` } })),
    { http: { method: "GET", path: "/api/provider" } },
  ],
})
