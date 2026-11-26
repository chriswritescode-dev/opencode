import { defineScenario } from "../lib/scenario"
import { restricted } from "../lib/restricted"

export default defineScenario({
  config: restricted,
  model: [
    {
      tool: { id: "call_write", name: "write", input: { path: "{{work}}/notes/a.txt", content: "alpha beta\n" } },
    },
    {
      tool: {
        id: "call_edit",
        name: "edit",
        input: { path: "{{work}}/notes/a.txt", oldString: "beta", newString: "gamma" },
      },
    },
    { tool: { id: "call_glob", name: "glob", input: { pattern: "notes/*.txt" } } },
    { tool: { id: "call_grep", name: "grep", input: { pattern: "gamma", path: "notes" } } },
    { tool: { id: "call_read", name: "read", input: { path: "{{work}}/notes/a.txt" } } },
    { text: "done" },
  ],
  steps: [
    { http: { method: "POST", path: "/api/session", body: {}, as: "s" } },
    {
      http: {
        method: "POST",
        path: "/api/session/{{s.data.id}}/prompt",
        body: { text: "write, edit, glob, grep and read notes/a.txt" },
      },
    },
    { waitEvent: { type: "session.execution.succeeded" } },
    { http: { method: "POST", path: "/api/experimental/session/{{s.data.id}}/wait" } },
    { http: { method: "GET", path: "/api/session/{{s.data.id}}/message" } },
  ],
})
