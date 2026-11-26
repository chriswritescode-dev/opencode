export const restricted = {
  plugins: [
    "-opencode.browser",
    "-opencode.goal",
    "-opencode.home-memory",
    "-opencode.tools",
    "-opencode.tool.automation",
    "-opencode.tool.machine",
    "-opencode.tool.artifact",
    "-opencode.tools.mcp-events",
    "-opencode.tools.mcp-resources",
    "-opencode.tool.session-search",
    "-opencode.tool.question",
    "-opencode.tool.shell",
    "-opencode.tool.subagent",
    "-opencode.tool.webfetch",
    "-opencode.tool.websearch",
    "-opencode.tool.todo",
    "-opencode.tool.image-generate",
    "-opencode.tool.video-generate",
  ],
  agents: {
    build: {
      permissions: [
        { action: "skill", resource: "*", effect: "deny" },
        { action: "execute", resource: "*", effect: "deny" },
      ],
    },
  },
}
