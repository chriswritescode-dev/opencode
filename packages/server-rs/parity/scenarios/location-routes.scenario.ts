import { defineScenario } from "../lib/scenario"

export default defineScenario({
  steps: [
    { http: { method: "GET", path: "/api/location" } },
    { http: { method: "GET", path: "/api/debug/location" } },
    {
      http: {
        method: "DELETE",
        path: "/api/debug/location",
        location: false,
        query: { "location[directory]": "{{work}}" },
      },
    },
    { http: { method: "GET", path: "/api/debug/location" } },
    { http: { method: "POST", path: "/api/location/reload" } },
    { http: { method: "GET", path: "/api/location" } },
    {
      http: {
        method: "GET",
        path: "/api/location",
        location: false,
        query: { "location[directory]": "{{work}}" },
      },
    },
    {
      http: {
        method: "GET",
        path: "/api/location",
        location: false,
        query: { "location[directory]": "{{work}}/missing" },
      },
    },
  ],
})
