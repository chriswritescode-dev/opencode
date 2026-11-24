import { defineScenario } from "../lib/scenario"

export default defineScenario({
  steps: [{ http: { method: "POST", path: "/api/debug/heap-dump", query: { directory: "relative/directory" } } }],
})
