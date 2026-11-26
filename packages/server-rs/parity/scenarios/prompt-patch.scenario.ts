import { defineScenario } from "../lib/scenario"
import { restricted } from "../lib/restricted"

const patchText = [
  "*** Begin Patch",
  "*** Add File: added.txt",
  "+added line",
  "*** Update File: hello.txt",
  "@@",
  "-hello parity",
  "+hello patched",
  "*** Delete File: remove.txt",
  "*** End Patch",
].join("\n")

export default defineScenario({
  files: { "hello.txt": "hello parity\n", "remove.txt": "remove me\n" },
  config: {
    ...restricted,
    model: "openai-test/patched-test",
    providers: {
      "openai-test": {
        models: {
          "patched-test": {
            name: "GPT Test",
            modelID: "gpt-5-test",
            capabilities: { tools: true, input: ["text"], output: ["text"] },
            cost: { input: 0, output: 0 },
            limit: { context: 100_000, output: 10_000 },
          },
        },
      },
    },
  },
  model: [{ tool: { id: "call_patch", name: "patch", input: { patchText } } }, { text: "done" }],
  steps: [
    { http: { method: "POST", path: "/api/session", body: {}, as: "s" } },
    { http: { method: "POST", path: "/api/session/{{s.data.id}}/prompt", body: { text: "apply the patch" } } },
    { waitEvent: { type: "session.execution.succeeded" } },
    { http: { method: "POST", path: "/api/experimental/session/{{s.data.id}}/wait" } },
    { http: { method: "GET", path: "/api/session/{{s.data.id}}/message" } },
  ],
})
