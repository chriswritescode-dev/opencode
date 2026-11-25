import { ConfigMarkdown } from "@opencode/core/config/markdown"
import { FSUtil } from "@opencode/util/fs-util"
import { LayerNode } from "@opencode/util/effect/layer-node"
import { Effect, Logger } from "effect"
import { mkdir, rm, symlink, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"

const area = process.argv[2]
if (area === "markdown") exportMarkdown()
else if (area === "scan") await exportScan()
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
