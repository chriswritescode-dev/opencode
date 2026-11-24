import { defineScenario } from "../lib/scenario"

export default defineScenario({
  files: { "opencode.json": '{\n  "shell": "/bin/sh"\n}\n' },
  steps: [
    { http: { method: "GET", path: "/api/config" } },
    { http: { method: "GET", path: "/api/config/shell" } },
    { http: { method: "PATCH", path: "/api/experimental/config", body: { shell: "/bin/bash" } } },
    { http: { method: "GET", path: "/api/config" } },
  ],
})
