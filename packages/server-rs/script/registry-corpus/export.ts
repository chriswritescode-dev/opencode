import { Agent } from "@opencode/core/agent"
import { Bus } from "@opencode/core/bus"
import { Command } from "@opencode/core/command"
import { Config } from "@opencode/core/config"
import { ConfigAgentPlugin } from "@opencode/core/config/plugin/agent"
import { ConfigCommandPlugin } from "@opencode/core/config/plugin/command"
import { ConfigMarkdown } from "@opencode/core/config/markdown"
import { AppNodeBuilder } from "@opencode/core/effect/app-node-builder"
import { Job } from "@opencode/core/job"
import { Location } from "@opencode/core/location"
import { Mcp } from "@opencode/core/mcp/index"
import { AgentPlugin } from "@opencode/core/plugin/agent"
import { CommandPlugin } from "@opencode/core/plugin/command"
import { PlanPlugin } from "@opencode/core/plugin/plan"
import { Project } from "@opencode/core/project"
import { AbsolutePath } from "@opencode/core/schema"
import { Session } from "@opencode/core/session"
import { SessionExecution } from "@opencode/core/session/execution"
import { SubagentJob } from "@opencode/core/session/subagent-job"
import { ShellSelect } from "@opencode/core/shell/select"
import { Directory, Document, Info } from "@opencode/schema/config"
import { SessionInbox } from "@opencode/schema/session-inbox"
import { SessionMessage } from "@opencode/schema/session-message"
import { FSUtil } from "@opencode/util/fs-util"
import { LayerNode } from "@opencode/util/effect/layer-node"
import { Global } from "@opencode/util/global"
import { AppProcess } from "@opencode/util/process"
import { DateTime, Effect, Layer, Logger, Schema } from "effect"
import { mkdir, mkdtemp, rm, symlink, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"
import { tempGlobalLayer } from "./fixture/global"
import { emptyConfigLayer, emptyMcpLayer, testLocationLayer } from "./fixture/mcp"
import { offlineModels } from "./fixture/models"
import { agentHost, host } from "./plugin/host"

const decodeConfig = Schema.decodeUnknownSync(Info)

const tool = {
  transform: () => Effect.die("unused tool.transform"),
  reload: () => Effect.die("unused tool.reload"),
  list: () => Effect.die("unused tool.list"),
  hook: () => Effect.succeed({ dispose: Effect.void }),
}

const shellLayer = Layer.succeed(
  ShellSelect.Service,
  ShellSelect.Service.of({
    resolve: () => Effect.succeed("sh"),
    transform: () => Effect.die("unused shell.transform"),
    reload: () => Effect.die("unused shell.reload"),
  }),
)

const area = process.argv[2]
if (area === "markdown") exportMarkdown()
else if (area === "scan") await exportScan()
else if (area === "agents") await exportAgents()
else if (area === "commands") await exportCommands()
else {
  console.error(`corpus: unknown area ${area ?? ""}`)
  process.exit(1)
}

function exportMarkdown() {
  const cases: Record<string, string> = {
    "invalid-colon-sanitize":
      "---\ndescription: Use when the user needs to crawl pages. Keywords: crawl, scrape\n---\nbody",
    "bracketed-tag": "---\ndescription: [Team] Does a thing\ntools: [read, edit]\n---\nbody",
    "flow-preserved": `---
description: Review changes. Keywords: review, audit
model: { providerID: example, model: chat, variant: high }
tools: { edit: false }
metadata: { "opencode/autoinvoke": false }
permissions: [{ action: edit, resource: "*", effect: deny }]
---
body`,
    "nested-flow-quoted": `---
description: Review changes. Keywords: review, audit
nested: [[read], [edit]]
tools: [read, edit] # comment: with colon
quoted: ["a]b", 'c''}d', don't]
---
body`,
    "multiline-flow": `---
description: Review changes. Keywords: review, audit
model: { providerID: example, # note: don't close } here
  model: chat,
  variant: high
} # trailing: comment
permissions: [{ action: edit,
resource: "*",
effect: deny }]
note: Remember: check tests
---
body`,
    "leading-tags": `---
description: [Team: Core] Does a thing
summary: {Team} Review changes
owner: [User's] Follow up
model: { providerID: example, model: chat }
---
body`,
    "crlf-flow":
      "---\r\ndescription: Review changes. Keywords: review, audit\r\nmodel: { providerID: example, model: chat }\r\n---\r\nbody",
    "block-scalar-header": `---
description: Review changes. Keywords: review, audit
prompt: |- # see: docs
  Review carefully.
---
body`,
    "unclosed-flow": "---\nmodel: { providerID: example\n---\nbody",
    "mismatched-flow": "---\ndescription: [Tag} text\n---\nbody",
    "plain-body": "just body",
    "empty-frontmatter": "---\n---\nbody",
    "frontmatter-no-close": "---\ntitle: Hi\nbody text",
    "comment-only": "---\n# just a comment\n---\nbody",
    "scalar-types": `---
enabled: true
disabled: false
count: 42
negative: -7
ratio: 1.5
whole: 1.0
exponent: 1e3
nothing: ~
empty: null
text: hello
quoted: "a: b"
single: 'it''s'
block: |
  line one
  line two
inline: [1, 2, 3]
---
body`,
    "nested-collections": `---
outer:
  inner:
    - one
    - two
  flag: yes
list:
  - name: first
    value: 1
  - name: second
    value: 2
---
body`,
    "quoted-colon": "---\ndescription: \"a: b\"\n---\nbody",
    "single-quoted-colon": "---\ndescription: 'a: b'\n---\nbody",
    "url-colon": "---\nurl: http://example.com/a\n---\nbody",
    "flow-with-comment": "---\ntools: [read, edit] # comment: with colon\n---\nbody",
    "key-with-hyphen": "---\nmy-key: a: b\n---\nbody",
    "indented-colon": "---\nnested:\n  value: a: b\n---\nbody",
    "comment-inside": "---\ndescription: Review changes. Keywords: review, audit\n# comment: kept\nmodel: { providerID: example }\n---\nbody",
    "empty-value": "---\ndescription:\nmodel: { providerID: example }\n---\nbody",
    "braced-tag": "---\nsummary: {Team} Review changes\n---\nbody",
    "trailing-open-brace": "---\nsummary: {Team Review changes\n---\nbody",
    "four-dashes": "----\ntitle: Hi\n---\nbody",
    "bom": "\uFEFF---\ntitle: Hi\n---\nbody",
    "crlf-plain": "---\r\ntitle: Hi\r\n---\r\nbody",
    "no-final-newline": "---\ntitle: Hi\n---",
    "tab-indent": "---\ntitle: Hi\n\tkey: a: b\n---\nbody",
    "blank-line": "---\ntitle: Hi\n\nmodel: { providerID: example }\n---\nbody",
  }

  const lines = Object.keys(cases)
    .sort()
    .map((name) => {
      const input = cases[name]!
      try {
        const parsed = ConfigMarkdown.parse(input)
        return JSON.stringify({ name, input, output: { data: parsed.data, content: parsed.content } })
      } catch {
        return JSON.stringify({ name, input, output: null })
      }
    })
  process.stdout.write(`[\n${lines.join(",\n")}\n]\n`)
}

type ScanFile = string | { type: "symlink"; target: string }

type ScanOperation =
  | {
      kind: "scan"
      root: string
      pattern: string
      options: { dot: boolean; symlink: boolean; include_files_only: boolean; ignore: ReadonlyArray<string> }
    }
  | { kind: "up"; names: ReadonlyArray<string>; start: string; stop: string | null }
  | { kind: "globUp"; pattern: string; start: string; stop: string | null }

type ScanCase = {
  name: string
  platform?: "unix"
  files: Record<string, ScanFile>
  operation: ScanOperation
}

async function exportScan() {
  const layer = LayerNode.compile(FSUtil.node)
  const base = path.join(tmpdir(), `opencode-registry-scan-${process.pid}-${Date.now()}`)
  const lines: Array<string> = []
  try {
    for (const item of scanCases()) {
      const root = path.join(base, item.name)
      await rm(root, { recursive: true, force: true })
      await mkdir(root, { recursive: true })
      for (const [file, value] of Object.entries(item.files)) {
        const target = path.join(root, file)
        await mkdir(path.dirname(target), { recursive: true })
        if (typeof value === "string") await writeFile(target, value)
        else await symlink(value.target, target)
      }
      const output = await runScan(layer, root, item.operation)
      lines.push(
        JSON.stringify({
          name: item.name,
          ...(item.platform === undefined ? {} : { platform: item.platform }),
          files: item.files,
          operation: item.operation,
          output,
        }),
      )
    }
  } finally {
    await rm(base, { recursive: true, force: true })
  }
  process.stdout.write(`[\n${lines.join(",\n")}\n]\n`)
}

async function runScan(layer: ReturnType<typeof LayerNode.compile>, root: string, operation: ScanOperation) {
  const program = Effect.gen(function* () {
    const fs = yield* FSUtil.Service
    const relative = (file: string) => path.relative(root, file).split(path.sep).join("/")
    if (operation.kind === "scan") {
      const files = yield* fs.scan(operation.pattern, {
        cwd: path.join(root, operation.root),
        absolute: true,
        include: operation.options.include_files_only ? "file" : "all",
        dot: operation.options.dot,
        symlink: operation.options.symlink,
        ignore: [...operation.options.ignore],
      })
      return files.map(relative).sort()
    }
    if (operation.kind === "up") {
      const files = yield* fs.up({
        targets: [...operation.names],
        start: path.join(root, operation.start),
        ...(operation.stop === null ? {} : { stop: path.join(root, operation.stop) }),
      })
      return files.map(relative)
    }
    const files = yield* fs.globUp(
      operation.pattern,
      path.join(root, operation.start),
      operation.stop === null ? undefined : path.join(root, operation.stop),
    )
    return files.map(relative)
  })
  return Effect.runPromise(program.pipe(Effect.provide(layer), Effect.provide(Logger.layer([]))))
}

function scanCases(): Array<ScanCase> {
  const tree: Record<string, string> = {
    ".git/HEAD": "head",
    ".hidden.md": "hidden",
    "a.md": "a",
    "ab.md": "ab",
    "agent/x.md": "x",
    "agents/y.md": "y",
    "b.md": "b",
    "command/a.md": "a",
    "commands/nested/b.md": "b",
    "deep/a/b/c.md": "c",
    "dir/one.txt": "one",
    "dir/two.md": "two",
    "mode/nested/three.md": "three",
    "mode/one.md": "one",
    "modes/two.md": "two",
    "node_modules/pkg/p.md": "p",
    "root.md": "root",
  }
  const nested: Record<string, string> = {
    "AGENTS.md": "agents",
    "CLAUDE.md": "claude",
    "nested/AGENTS.md": "nested agents",
    "nested/CLAUDE.md": "nested claude",
    "nested/deep/keep.txt": "keep",
  }
  const markdown: Record<string, string> = {
    "nested/deep/one.md": "one",
    "nested/two.md": "two",
    "root.md": "root",
  }
  const globstarUp: Record<string, string> = {
    "nested/deep/keep.txt": "keep",
    "root.md": "root",
  }
  return [
    {
      name: "braces-agent-globstar",
      files: tree,
      operation: {
        kind: "scan",
        root: "",
        pattern: "{agent,agents}/**/*.md",
        options: { dot: true, symlink: false, include_files_only: true, ignore: [] },
      },
    },
    {
      name: "braces-command-globstar",
      files: tree,
      operation: {
        kind: "scan",
        root: "",
        pattern: "{command,commands}/**/*.md",
        options: { dot: true, symlink: false, include_files_only: true, ignore: [] },
      },
    },
    {
      name: "braces-mode-direct",
      files: tree,
      operation: {
        kind: "scan",
        root: "",
        pattern: "{mode,modes}/*.md",
        options: { dot: true, symlink: false, include_files_only: true, ignore: [] },
      },
    },
    {
      name: "char-class",
      files: tree,
      operation: {
        kind: "scan",
        root: "",
        pattern: "[ab]*.md",
        options: { dot: false, symlink: false, include_files_only: true, ignore: [] },
      },
    },
    {
      name: "dot-false-top-level",
      files: tree,
      operation: {
        kind: "scan",
        root: "",
        pattern: "*.md",
        options: { dot: false, symlink: false, include_files_only: true, ignore: [] },
      },
    },
    {
      name: "globUp-globstar",
      files: globstarUp,
      operation: { kind: "globUp", pattern: "**/*.md", start: "nested", stop: "." },
    },
    {
      name: "globUp-star",
      files: markdown,
      operation: { kind: "globUp", pattern: "*.md", start: "nested/deep", stop: "." },
    },
    {
      name: "globstar-all-ignore",
      files: tree,
      operation: {
        kind: "scan",
        root: "",
        pattern: "**/*",
        options: { dot: true, symlink: false, include_files_only: true, ignore: FSUtil.ignoreGlobs },
      },
    },
    {
      name: "globstar-files-and-directories",
      files: tree,
      operation: {
        kind: "scan",
        root: "",
        pattern: "**/*",
        options: { dot: true, symlink: false, include_files_only: false, ignore: FSUtil.ignoreGlobs },
      },
    },
    {
      name: "globstar-markdown-node-modules-ignore",
      files: tree,
      operation: {
        kind: "scan",
        root: "",
        pattern: "**/*.md",
        options: {
          dot: true,
          symlink: false,
          include_files_only: true,
          ignore: ["node_modules", "**/node_modules/**"],
        },
      },
    },
    {
      name: "question",
      files: tree,
      operation: {
        kind: "scan",
        root: "",
        pattern: "?.md",
        options: { dot: false, symlink: false, include_files_only: true, ignore: [] },
      },
    },
    {
      name: "scan-symlink-follow",
      platform: "unix",
      files: { "real/secret.md": "secret", link: { type: "symlink", target: "real" } },
      operation: {
        kind: "scan",
        root: "",
        pattern: "link/**/*.md",
        options: { dot: true, symlink: true, include_files_only: true, ignore: [] },
      },
    },
    {
      name: "scan-symlink-globstar-no-follow",
      platform: "unix",
      files: {
        "f.md": "f",
        "real/s.md": "secret",
        link: { type: "symlink", target: "real" },
      },
      operation: {
        kind: "scan",
        root: "",
        pattern: "**/*",
        options: { dot: true, symlink: false, include_files_only: true, ignore: [] },
      },
    },
    {
      name: "scan-symlink-literal-globstar",
      platform: "unix",
      files: { "real/s.md": "secret", link: { type: "symlink", target: "real" } },
      operation: {
        kind: "scan",
        root: "",
        pattern: "link/**/*.md",
        options: { dot: true, symlink: false, include_files_only: true, ignore: [] },
      },
    },
    {
      name: "scan-symlink-literal-segment",
      platform: "unix",
      files: { "real/s.md": "secret", link: { type: "symlink", target: "real" } },
      operation: {
        kind: "scan",
        root: "",
        pattern: "link/*.md",
        options: { dot: true, symlink: false, include_files_only: true, ignore: [] },
      },
    },
    {
      name: "up-names",
      files: nested,
      operation: { kind: "up", names: ["AGENTS.md", "CLAUDE.md"], start: "nested/deep", stop: "." },
    },
    {
      name: "up-stop",
      files: nested,
      operation: { kind: "up", names: ["AGENTS.md", "CLAUDE.md"], start: "nested/deep", stop: "nested" },
    },
  ].sort((a, b) => (a.name < b.name ? -1 : 1))
}

type AgentCase = {
  name: string
  home: string
  files?: Record<string, string>
  documents?: Array<unknown>
}

function agentLayer(home: string) {
  const globalLayer = Layer.succeed(Global.Service, Global.Service.of(Global.make({ home })))
  const location = new Location.Info({
    directory: AbsolutePath.make("/workspace"),
    project: {
      id: Project.ID.global,
      directory: AbsolutePath.make("/workspace"),
      canonical: AbsolutePath.make("/workspace"),
    },
  })
  const locationLayer = Layer.succeed(Location.Service, Location.Service.of(location))
  return AppNodeBuilder.build(
    LayerNode.group([Agent.node, Bus.node, FSUtil.node, Location.node, Global.node]),
    [Global.node.replace(globalLayer), Location.node.replace(locationLayer)],
  )
}

async function exportAgents() {
  const cases: AgentCase[] = [
    {
      name: "config-legacy-files",
      home: "/home/test",
      files: {
        "agents/reviewer.md": `---
model: openrouter/openai/gpt-5
description: Markdown description
temperature: 0.5
tools:
  write: false
---
Review carefully.`,
        "agents/team/helper.md": "Help the team.",
        "agents/team/deep/helper.md": "Deeper helper.",
        "agents/native.md": `---
variant: high
request:
  headers:
    x-agent: native
  body:
    effort: high
permissions:
  - action: edit
    resource: "*"
    effect: deny
---
Use native v2 fields.`,
        "agents/themed.md": `---
description: Themed
mode: subagent
color: accent
---
Use a theme color.`,
        "agents/disabled.md": `---
disabled: true
---
Disabled`,
        "agents/empty.md": "",
        "agents/.opencode/node_modules/probe/README.md": "# Dependency README",
        "modes/plan.md": "Make a plan.",
      },
      documents: [{ agents: { reviewer: { description: "JSON description" } } }],
    },
    {
      name: "config-v2-fields",
      home: "/home/test",
      files: {
        "agents/named.md": `---
name: named
description: Named reviewer
permissions:
  - action: edit
    resource: "*"
    effect: deny
---
Review only.`,
        "agents/mixed.md": `---
tools:
  shell: false
permissions:
  - action: edit
    resource: "*"
    effect: deny
---
Mixed fields.`,
      },
    },
    {
      name: "config-markdown-model-variants",
      home: "/home/test",
      files: {
        "agents/separate.md": `---
model: example/chat
variant: high
---
Body separate.`,
        "agents/unqualified.md": `---
model: example/chat
---
Body unqualified.`,
        "agents/embedded.md": `---
model: example/chat#high
---
Body embedded.`,
        "agents/structured.md": `---
model: { providerID: example, model: chat, variant: high }
---
Body structured.`,
        "agents/structured-plain.md": `---
model: { providerID: example, model: chat }
---
Body structured-plain.`,
        "agents/standalone.md": `---
variant: high
---
Body standalone.`,
        "agents/embedded-ignored.md": `---
model: example/chat#high
variant: low
---
Body embedded-ignored.`,
        "agents/structured-ignored.md": `---
model: { providerID: example, model: chat, variant: high }
variant: low
---
Body structured-ignored.`,
        "agents/native.md": `---
model: example/chat
variant: high
request:
  headers:
    x-agent: native
  body:
    effort: high
permissions:
  - action: edit
    resource: "*"
    effect: deny
---
Native variant.`,
      },
    },
    {
      name: "config-request-merge",
      home: "/home/test",
      documents: [
        {
          agents: {
            reviewer: {
              model: "anthropic/claude-sonnet",
              system: "Review carefully.",
              description: "Reviews changes",
              mode: "subagent",
              hidden: true,
              color: "#ff6b6b",
              steps: 12,
              request: {
                headers: { first: "one", shared: "first" },
                body: { enabled: true, profile: "review", effort: "medium" },
              },
            },
          },
        },
        {
          agents: {
            reviewer: {
              request: {
                headers: { shared: "last", second: "two" },
                body: { retries: 2, effort: "high" },
              },
            },
          },
        },
      ],
    },
    {
      name: "config-global-permissions",
      home: "/home/test",
      documents: [
        {
          permissions: [
            { action: "question", resource: "*", effect: "allow" },
            { action: "bash", resource: "*", effect: "ask" },
            { action: "read", resource: "*", effect: "allow" },
            { action: "read", resource: "~/.local/share/opencode/mcp-auth.json", effect: "deny" },
            { action: "external_directory", resource: "~/.local/share/opencode/*", effect: "deny" },
            { action: "external_directory", resource: "~/p/**", effect: "allow" },
            { action: "external_directory", resource: "/some/~/path", effect: "deny" },
            { action: "external_directory", resource: "$HOMELESS/**", effect: "deny" },
            { action: "shell", resource: "$HOME/private/**", effect: "deny" },
            { action: "edit", resource: "$HOME/private/**", effect: "deny" },
          ],
          agents: {
            reviewer: {
              description: "Home relative",
              permissions: [{ action: "edit", resource: "*", effect: "deny" }],
            },
          },
        },
      ],
    },
    {
      name: "config-windows-permissions",
      home: "C:\\Users\\test",
      documents: [
        {
          permissions: [
            { action: "external_directory", resource: "~/p/**", effect: "allow" },
            { action: "external_directory", resource: "$HOME/cache/**", effect: "deny" },
            { action: "shell", resource: "$HOME/private/**", effect: "deny" },
          ],
          agents: {
            reviewer: { permissions: [{ action: "edit", resource: "*", effect: "deny" }] },
          },
        },
      ],
    },
    {
      name: "config-default-disabled",
      home: "/home/test",
      documents: [
        {
          default_agent: "reviewer",
          agents: {
            reviewer: { description: "Reviews changes", mode: "primary" },
            general: { disabled: true },
          },
        },
      ],
    },
    {
      name: "config-legacy-permission-order",
      home: "/home/test",
      files: {
        "agents/legacy-permissions.md": `---
model: example/chat
temperature: 0.1
permission:
  "*": allow
  bash: ask
  edit: deny
---
Legacy precedence.`,
        "agents/legacy-color.md": `---
temperature: 0.2
color: accent
description: Legacy color
---
Legacy color.`,
      },
    },
  ]
  const output: Array<Record<string, unknown>> = [await exportBuiltins()]
  for (const item of cases) output.push(await exportAgentCase(item))
  process.stdout.write(`[\n${output.map((item) => JSON.stringify(item)).join(",\n")}\n]\n`)
}

async function exportBuiltins() {
  const home = "{{home}}"
  const program = Effect.gen(function* () {
    const agent = yield* Agent.Service
    yield* AgentPlugin.Plugin.effect(host({ agent: agentHost(agent), tool }))
    yield* PlanPlugin.Plugin.effect(
      host({
        agent: agentHost(agent),
        tool,
        session: { hook: () => Effect.succeed({ dispose: Effect.void }) },
      }),
    )
    return {
      name: "builtins",
      home,
      list: yield* agent.list(),
      select: yield* agent.select(),
      resolve: yield* agent.resolve(),
    }
  })
  return Effect.runPromise(
    program.pipe(Effect.scoped, Effect.provide(agentLayer(home)), Effect.provide(Logger.layer([]))),
  )
}

async function exportAgentCase(item: AgentCase) {
  const root = await mkdtemp(path.join(tmpdir(), "opencode-agent-case-"))
  try {
    for (const [file, content] of Object.entries(item.files ?? {})) {
      const target = path.join(root, file)
      await mkdir(path.dirname(target), { recursive: true })
      await writeFile(target, content)
    }
    const entries: Array<Document | Directory> = (item.documents ?? []).map(
      (document) => new Document({ type: "document", info: decodeConfig(document) }),
    )
    if (item.files !== undefined) {
      entries.push(new Directory({ type: "directory", path: AbsolutePath.make(root) }))
    }
    const program = Effect.gen(function* () {
      const agent = yield* Agent.Service
      yield* AgentPlugin.Plugin.effect(host({ agent: agentHost(agent), tool }))
      yield* PlanPlugin.Plugin.effect(
        host({
          agent: agentHost(agent),
          tool,
          session: { hook: () => Effect.succeed({ dispose: Effect.void }) },
        }),
      )
      yield* ConfigAgentPlugin.Plugin.effect(host({ agent: agentHost(agent) })).pipe(
        Effect.provide(Config.testLayer(entries)),
      )
      return {
        name: item.name,
        home: item.home,
        ...(item.files === undefined ? {} : { files: item.files }),
        ...(item.documents === undefined ? {} : { documents: item.documents }),
        list: yield* agent.list(),
      }
    })
    return await Effect.runPromise(
      program.pipe(Effect.scoped, Effect.provide(agentLayer(item.home)), Effect.provide(Logger.layer([]))),
    )
  } finally {
    await rm(root, { recursive: true, force: true })
  }
}

type CommandCase = {
  name: string
  files?: Record<string, string>
  documents?: Array<unknown>
}

function commandLayer() {
  return AppNodeBuilder.build(
    LayerNode.group([
      Command.node,
      Bus.node,
      FSUtil.node,
      AppProcess.node,
      Location.node,
      ShellSelect.node,
      Session.node,
      Job.node,
      SubagentJob.node,
      Agent.node,
      Mcp.node,
    ]),
    [
      Mcp.node.replace(emptyMcpLayer),
      Config.node.replace(emptyConfigLayer),
      Location.node.replace(testLocationLayer),
      ShellSelect.node.replace(shellLayer),
      Global.node.replace(tempGlobalLayer),
      SessionExecution.node.replace(SessionExecution.noopLayer),
      offlineModels,
    ],
  )
}

function commandHost(command: Command.Interface) {
  return { list: () => command.list(), transform: command.transform, reload: command.reload }
}

async function exportCommands() {
  const output = [
    { name: "templates", templates: await exportCommandTemplates() },
    await exportCommandCase({ name: "builtins" }),
    await exportCommandCase({
      name: "config-files",
      files: {
        "command/legacy.md": "Legacy body",
        "commands/review.md":
          "---\ndescription: File review\nagent: reviewer\nmodel: example/chat#high\nsubtask: true\n---\nReview files",
        "commands/nested/docs.md": "Write docs",
        "commands/empty.md": "",
        "commands/.hidden.md": "Hidden body",
        "commands/broken.md": "---\ndescription: 5\n---\nBroken",
        "commands/ignore.txt": "ignored",
      },
    }),
    await exportCommandCase({
      name: "config-inline",
      documents: [
        { commands: { explain: { template: "Explain $ARGUMENTS.", description: "Explain" } } },
        {
          commands: {
            init: { template: "Custom init", description: "Custom init" },
            nested: { template: "Nested", description: "Nested desc" },
          },
        },
      ],
    }),
    await exportCommandCase({
      name: "config-variants",
      files: {
        "commands/separate.md": "---\nmodel: example/chat\nvariant: high\n---\nSeparate",
        "commands/embedded.md": "---\nmodel: example/chat#low\nvariant: high\n---\nEmbedded",
        "commands/structured.md":
          "---\nmodel: { providerID: example, model: chat, variant: high }\n---\nStructured",
      },
    }),
  ]
  process.stdout.write(`[\n${output.map((item) => JSON.stringify(item)).join(",\n")}\n]\n`)
}

async function exportCommandCase(item: CommandCase) {
  const root = await mkdtemp(path.join(tmpdir(), "opencode-command-case-"))
  try {
    for (const [file, content] of Object.entries(item.files ?? {})) {
      const target = path.join(root, file)
      await mkdir(path.dirname(target), { recursive: true })
      await writeFile(target, content)
    }
    const entries: Array<Document | Directory> = (item.documents ?? []).map(
      (document) => new Document({ type: "document", info: decodeConfig(document) }),
    )
    if (item.files !== undefined) {
      entries.push(new Directory({ type: "directory", path: AbsolutePath.make(root) }))
    }
    const program = Effect.gen(function* () {
      const command = yield* Command.Service
      yield* CommandPlugin.Plugin.effect(host({ command: commandHost(command) }))
      yield* ConfigCommandPlugin.Plugin.effect(host({ command: commandHost(command) })).pipe(
        Effect.provide(Config.testLayer(entries)),
      )
      return {
        name: item.name,
        ...(item.files === undefined ? {} : { files: item.files }),
        ...(item.documents === undefined ? {} : { documents: item.documents }),
        list: yield* command.list(),
      }
    })
    return await Effect.runPromise(
      program.pipe(Effect.scoped, Effect.provide(commandLayer()), Effect.provide(Logger.layer([]))),
    )
  } finally {
    await rm(root, { recursive: true, force: true })
  }
}

async function exportCommandTemplates() {
  const cases = [
    ...["$&", "$$", "$`", "$'"].flatMap((input) => [
      { template: "Explain $ARGUMENTS.", input, expected: `Explain ${input}.` },
      { template: "Explain $1.", input: `"${input}"`, expected: `Explain ${input}.` },
      { template: "Explain.", input, expected: `Explain.\n\n${input}` },
    ]),
    ...["abc", "", "alpha beta", '"alpha beta"', "$1", "$<name>"].map((input) => ({
      template: "Explain $ARGUMENTS.",
      input,
      expected: `Explain ${input}.`,
    })),
    {
      template: "First $1. Rest $2.",
      input: '"alpha beta" gamma delta',
      expected: "First alpha beta. Rest gamma delta.",
    },
    {
      template: "Run awk '{print $0}' on $1.",
      input: "app.log",
      expected: "Run awk '{print $0}' on app.log.",
    },
    {
      template: "Run awk '{print $0}'.",
      input: "app.log",
      expected: "Run awk '{print $0}'.\n\napp.log",
    },
    {
      template: "First $1. Rest $2.",
      input: '"$ARGUMENTS" tail',
      expected: "First $ARGUMENTS. Rest tail.",
    },
    { template: "$ARGUMENTS / $ARGUMENTS", input: "$& $$", expected: "$& $$ / $& $$" },
  ].map((item, index) => ({ name: `case-${index}`, ...item }))
  const document = new Document({
    type: "document",
    info: decodeConfig({
      commands: Object.fromEntries(
        cases.map((item, index) => [`case${index}`, { template: item.template }]),
      ),
    }),
  })
  const prompts: string[] = []
  const program = Effect.gen(function* () {
    const command = yield* Command.Service
    yield* ConfigCommandPlugin.Plugin.effect(
      host({
        command: commandHost(command),
        session: {
          prompt: (input) =>
            Effect.sync(() => {
              prompts.push(input.text)
              return SessionInbox.User.make({
                id: SessionMessage.ID.make("msg_test"),
                sessionID: input.sessionID,
                time: { created: DateTime.makeUnsafe(0) },
                type: "user",
                payload: { text: input.text },
                delivery: input.delivery ?? "steer",
              })
            }),
        },
      }),
    ).pipe(Effect.provide(Config.testLayer([document])))
    for (const [index, item] of cases.entries()) {
      yield* command.execute({
        name: `case${index}`,
        invocation: {
          sessionID: Session.ID.make("ses_test"),
          prompt: { text: item.input },
          delivery: "steer",
        },
      })
    }
    return cases.map((item, index) => ({
      name: item.name,
      template: item.template,
      input: item.input,
      expected: prompts[index],
    }))
  })
  return await Effect.runPromise(
    program.pipe(Effect.scoped, Effect.provide(commandLayer()), Effect.provide(Logger.layer([]))),
  )
}
