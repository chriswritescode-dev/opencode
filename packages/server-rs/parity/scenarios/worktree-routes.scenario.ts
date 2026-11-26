import { defineScenario } from "../lib/scenario"

export default defineScenario({
  setup: [
    { files: { "README.md": "# Repo\n", "keep.txt": "keep\n" } },
    { git: { args: ["init", "-q", "-b", "main"] } },
    { git: { args: ["add", "-A"] } },
    { git: { args: ["commit", "-qm", "root"] } },
  ],
  steps: [
    { http: { method: "GET", path: "/api/vcs", as: "info" } },
    { http: { method: "GET", path: "/api/project", as: "projects" } },
    {
      http: {
        method: "GET",
        path: "/api/worktree",
        query: { projectID: "{{projects.0.id}}" },
        as: "initial",
      },
    },
    {
      http: {
        method: "POST",
        path: "/api/worktree",
        body: { projectID: "{{projects.0.id}}", directory: "{{work}}/trees", name: "linked" },
        as: "created",
      },
    },
    { git: { args: ["rev-parse", "HEAD"] } },
    {
      http: {
        method: "POST",
        path: "/api/worktree",
        body: { projectID: "{{projects.0.id}}", directory: "{{work}}/trees", name: "linked" },
        as: "collision",
      },
    },
    { files: { "trees/linked/dirty.txt": "dirty\n" } },
    {
      http: {
        method: "DELETE",
        path: "/api/worktree",
        body: { projectID: "{{projects.0.id}}", directory: "{{created.directory}}", force: false },
      },
    },
    {
      http: {
        method: "DELETE",
        path: "/api/worktree",
        body: { projectID: "{{projects.0.id}}", directory: "{{created.directory}}", force: true },
      },
    },
    {
      http: {
        method: "DELETE",
        path: "/api/worktree",
        body: { projectID: "{{projects.0.id}}", directory: "{{collision.directory}}", force: true },
      },
    },
    { git: { args: ["worktree", "add", "--detach", "{{root}}/external", "HEAD"] } },
    {
      http: {
        method: "POST",
        path: "/api/worktree/refresh",
        body: { projectID: "{{projects.0.id}}" },
      },
    },
    {
      http: {
        method: "GET",
        path: "/api/worktree",
        query: { projectID: "{{projects.0.id}}" },
        as: "final",
      },
    },
    { http: { method: "GET", path: "/api/worktree", query: { projectID: "unknown" } } },
  ],
})
