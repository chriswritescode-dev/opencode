import { defineScenario } from "../lib/scenario"

export default defineScenario({
  steps: [
    {
      http: {
        method: "POST",
        path: "/api/credential",
        body: { integrationID: "openai-test", label: "First", value: { type: "key", key: "first-key" } },
        as: "first",
      },
    },
    { settle: true },
    {
      http: {
        method: "POST",
        path: "/api/credential",
        body: {
          id: "{{first.data.id}}",
          integrationID: "openai-test",
          label: "Conflict",
          value: { type: "key", key: "conflict-key" },
        },
      },
    },
    { http: { method: "GET", path: "/api/credential", as: "listed" } },
    { http: { method: "PATCH", path: "/api/credential/{{first.data.id}}", body: { label: "Renamed" } } },
    { settle: true },
    { http: { method: "GET", path: "/api/credential" } },
    {
      http: {
        method: "POST",
        path: "/api/credential",
        body: { integrationID: "openai-test", label: "Second", value: { type: "key", key: "second-key" } },
        as: "second",
      },
    },
    { settle: true },
    { http: { method: "POST", path: "/api/credential/{{first.data.id}}/activate" } },
    { settle: true },
    { http: { method: "DELETE", path: "/api/credential/{{first.data.id}}" } },
    { settle: true },
    { http: { method: "GET", path: "/api/credential", as: "final" } },
  ],
})
