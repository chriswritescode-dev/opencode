import { defineScenario } from "../lib/scenario"

export default defineScenario({
  setup: [
    { files: { "hello.txt": "hello\n" } },
    { git: { args: ["init", "-q", "-b", "main"] } },
    { git: { args: ["add", "-A"] } },
    { git: { args: ["commit", "-qm", "initial"] } },
    { git: { args: ["checkout", "-qb", "feature", "main"] } },
    { files: { "hello.txt": "feature\n", "feature.txt": "feature\n" } },
    { git: { args: ["add", "-A"] } },
    { git: { args: ["commit", "-qm", "feature"] } },
    { files: { "hello.txt": "dirty\n", "drafts/one.txt": "one\n", "drafts/two.txt": "two\n" } },
  ],
  steps: [
    { http: { method: "GET", path: "/api/vcs", as: "info" } },
    { http: { method: "GET", path: "/api/vcs/status", as: "status" } },
    { http: { method: "GET", path: "/api/vcs/branch", as: "branches" } },
    { http: { method: "GET", path: "/api/vcs/branch", query: { search: "fea" }, as: "branchSearch" } },
    { http: { method: "GET", path: "/api/vcs/branch", query: { limit: "1" }, as: "branchLimit" } },
    { http: { method: "GET", path: "/api/vcs/base", as: "base" } },
    { http: { method: "POST", path: "/api/vcs/init", as: "initConflict" } },
    {
      http: {
        method: "GET",
        path: "/api/vcs/diff",
        query: { mode: "working", context: "1" },
        as: "diffWorkingContext",
      },
    },
    {
      http: {
        method: "GET",
        path: "/api/vcs/diff",
        query: { mode: "working", untracked: "folders" },
        as: "diffWorkingFolders",
      },
    },
    { http: { method: "GET", path: "/api/vcs/diff", query: { mode: "branch" }, as: "diffBranch" } },
    { http: { method: "GET", path: "/api/vcs/diff", query: { mode: "committed" }, as: "diffCommitted" } },
  ],
})
