import { defineScenario } from "../lib/scenario"

export default defineScenario({
  steps: [
    {
      http: {
        method: "POST",
        path: "/api/session",
        body: { "title": "parity" },
        as: "first",
      },
    },
    {
      http: {
        method: "POST",
        path: "/api/session",
        body: {},
        as: "second",
      },
    },
    { waitEvent: { type: "session.created" } },
    { waitEvent: { type: "session.created" } },
    { http: { method: "GET", path: "/api/session" } },
    { http: { method: "GET", path: "/api/session/{{first.data.id}}" } },
  ],
})
