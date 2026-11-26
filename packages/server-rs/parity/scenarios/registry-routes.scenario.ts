import { defineScenario } from "../lib/scenario"

export default defineScenario({
  files: {
    "AGENTS.md": "# Parity project\n\nFixture project for the registry routes.\n",
    "docs/README.md": "# Docs\n\nLocal reference source.\n",
    ".opencode/agents/reviewer.md":
      "---\ndescription: Reviews code changes\nmode: subagent\n---\nYou review code changes and report findings.\n",
    ".opencode/agent/legacy.md":
      "---\ndescription: Legacy reviewer\nmode: subagent\ntools:\n  write: false\n---\nYou review legacy code.\n",
    ".opencode/commands/hello.md": "---\ndescription: Say hello\n---\nSay hello to $ARGUMENTS\n",
    ".opencode/skills/demo/SKILL.md":
      "---\nname: demo\ndescription: Demonstrates a project skill\n---\nRead the demo notes before answering.\n",
  },
  config: {
    plugins: ["-opencode.browser", "-opencode.goal", "-opencode.home-memory"],
    automations: false,
    default_agent: "reviewer",
    agents: {
      helper: { description: "Inline helper agent", mode: "subagent" },
    },
    commands: {
      inline: { description: "Inline command", template: "Inline $ARGUMENTS" },
    },
    references: {
      docs: { path: "docs" },
      mono: { repository: "owner/mono", branch: "main" },
    },
  },
  steps: [
    { http: { method: "GET", path: "/api/agent" } },
    { http: { method: "GET", path: "/api/agent/reviewer" } },
    { http: { method: "GET", path: "/api/agent/unknown-reviewer" } },
    { http: { method: "GET", path: "/api/command" } },
    { http: { method: "GET", path: "/api/skill" } },
    { http: { method: "GET", path: "/api/reference" } },
  ],
})
