import { defineScenario } from "../lib/scenario"

export default defineScenario({
  files: { ".opencode/plugins/README.md": "plugins\n" },
  steps: [
    { http: { method: "GET", path: "/api/plugin" } },
    { files: { ".opencode/plugins/smoke.ts": 'export default { id: "smoke", setup: async () => {} }\n' } },
    { http: { method: "POST", path: "/api/location/reload" } },
    { waitEvent: { type: "plugin.updated" } },
    { http: { method: "GET", path: "/api/plugin" } },
    { http: { method: "GET", path: "/api/plugin/gui", query: { id: "smoke" } } },
  ],
})
