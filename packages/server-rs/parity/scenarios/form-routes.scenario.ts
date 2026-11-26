import { defineScenario } from "../lib/scenario"

const config = {
  plugins: ["-opencode.browser", "-opencode.goal", "-opencode.home-memory"],
  automations: false,
}

export default defineScenario({
  config,
  steps: [
    {
      http: {
        method: "POST",
        path: "/api/session/global/form",
        body: {
          id: "frm_demo",
          title: "Pick a name",
          metadata: { origin: "parity" },
          fields: [{ key: "name", type: "string", title: "Name", required: true }],
        },
        as: "created",
      },
    },
    {
      http: {
        method: "POST",
        path: "/api/session/global/form",
        body: {
          id: "frm_demo",
          title: "Pick a name",
          fields: [{ key: "name", type: "string", title: "Name", required: true }],
        },
      },
    },
    {
      http: {
        method: "POST",
        path: "/api/session/global/form",
        body: {
          id: "frm_bad",
          title: "Bad condition",
          fields: [
            { key: "first", type: "string", title: "First" },
            { key: "second", type: "string", title: "Second", when: [{ key: "first", op: "eq", value: "yes" }] },
          ],
        },
      },
    },
    { http: { method: "GET", path: "/api/session/global/form" } },
    { http: { method: "GET", path: "/api/form" } },
    {
      http: { method: "GET", path: "/api/session/global/form/{{created.data.id}}" },
    },
    {
      http: {
        method: "POST",
        path: "/api/session/global/form/{{created.data.id}}/reply",
        body: { answer: {} },
      },
    },
    {
      http: {
        method: "POST",
        path: "/api/session/global/form/{{created.data.id}}/reply",
        body: { answer: { name: "Ada" } },
      },
    },
    {
      http: {
        method: "POST",
        path: "/api/session/global/form/{{created.data.id}}/reply",
        body: { answer: { name: "Grace" } },
      },
    },
    {
      http: {
        method: "POST",
        path: "/api/session/global/form",
        body: {
          id: "frm_cancel",
          title: "Confirm",
          fields: [{ key: "ok", type: "boolean", title: "OK", required: true }],
        },
      },
    },
    {
      http: {
        method: "DELETE",
        path: "/api/session/global/form/frm_cancel",
        query: { message: "Not needed any more" },
      },
    },
    { http: { method: "GET", path: "/api/session/global/form/frm_cancel" } },
  ],
})
