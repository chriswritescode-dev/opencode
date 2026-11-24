import { defineScenario } from "../lib/scenario"

export default defineScenario({
  files: { "hello.txt": "hello parity\n" },
  steps: [
    { http: { method: "GET", path: "/api/fs/read/hello.txt" } },
    { http: { method: "GET", path: "/api/fs/read/missing.txt" } },
    { http: { method: "GET", path: "/api/fs/list", query: { "path": "." } } },
  ],
})
