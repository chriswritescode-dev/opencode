import { defineScenario } from "../lib/scenario"

export default defineScenario({
  steps: [
    { http: { method: "GET", path: "/api/project", as: "projects" } },
    {
      http: {
        method: "PATCH",
        path: "/api/project/{{projects.0.id}}",
        body: { name: "Parity Project", icon: { color: "#ff0000" }, commands: { start: "echo start" } },
      },
    },
    { http: { method: "PATCH", path: "/api/project/{{projects.0.id}}", body: { name: "" } } },
    { http: { method: "PATCH", path: "/api/project/unknown-project", body: { name: "unknown" } } },
    { http: { method: "GET", path: "/api/project", query: { archived: "true" } } },
    { http: { method: "GET", path: "/api/project", query: { archived: "maybe" } } },
  ],
})
