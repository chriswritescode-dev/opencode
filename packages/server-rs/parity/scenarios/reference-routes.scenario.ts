import { defineScenario } from "../lib/scenario"

export default defineScenario({
  files: {
    "docs/README.md": "# Docs\n",
    "notes/note.md": "note\n",
    "keep.txt": "keep\n",
  },
  config: {
    references: {
      docs: "./docs",
      notes: { path: "./notes", description: "Team notes", hidden: true },
      remote: { repository: "example/reference" },
    },
  },
  env: { OPENCODE_REPO_CLONE_GITHUB_BASE_URL: "file://{{root}}/origin" },
  setup: [{ git: { args: ["init", "--bare", "-q", "{{root}}/origin/example/reference.git"] } }],
  steps: [{ http: { method: "GET", path: "/api/reference", as: "references" } }],
})
