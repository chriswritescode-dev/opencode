import { defineScenario } from "../lib/scenario"

export default defineScenario({
  files: { "sub/a.txt": "a\n" },
  steps: [
    { http: { method: "GET", path: "/api/location" } },
    { http: { method: "POST", path: "/api/location/reload" } },
    { waitEvent: { type: "plugin.updated" } },
    {
      http: {
        method: "GET",
        path: "/api/location",
        location: false,
        query: { "location[directory]": "{{work}}/sub" },
      },
    },
    { waitEvent: { type: "plugin.updated" } },
  ],
})
