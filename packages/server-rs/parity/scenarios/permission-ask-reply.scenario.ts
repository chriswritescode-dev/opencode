import { defineScenario } from "../lib/scenario"

export default defineScenario({
  config: {
    plugins: ["-opencode.browser", "-opencode.goal", "-opencode.home-memory"],
    automations: false,
    permissions: [{ action: "bash", resource: "*", effect: "ask" }],
  },
  steps: [
    { http: { method: "POST", path: "/api/session", body: {}, as: "session" } },
    {
      http: {
        method: "POST",
        path: "/api/session/{{session.data.id}}/permission",
        body: { action: "bash", resources: ["ls"], save: ["ls *"] },
        as: "request",
      },
    },
    { http: { method: "GET", path: "/api/session/{{session.data.id}}/permission" } },
    { http: { method: "GET", path: "/api/permission/request" } },
    { http: { method: "GET", path: "/api/session/{{session.data.id}}/permission/{{request.data.id}}" } },
    {
      http: {
        method: "POST",
        path: "/api/session/{{session.data.id}}/permission/{{request.data.id}}/reply",
        body: { decision: "always" },
      },
    },
    { http: { method: "GET", path: "/api/permission/saved", as: "saved" } },
    {
      http: {
        method: "POST",
        path: "/api/session/{{session.data.id}}/permission",
        body: { action: "bash", resources: ["ls -la"] },
      },
    },
    {
      http: {
        method: "POST",
        path: "/api/session/{{session.data.id}}/permission/per_unknown/reply",
        body: { decision: "once" },
      },
    },
    {
      http: {
        method: "PATCH",
        path: "/api/permission/saved/{{saved.data.0.id}}",
        body: { resource: "ls -lh" },
      },
    },
    { http: { method: "DELETE", path: "/api/permission/saved/{{saved.data.0.id}}" } },
  ],
})
