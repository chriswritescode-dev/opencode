import fs from "node:fs/promises"
import os from "node:os"
import path from "node:path"
import { createTwoFilesPatch, diffLines } from "diff"
import { Cause, Effect, Exit, Layer, Option, Stream } from "effect"
import { Agent } from "@opencode/core/agent"
import { FileAccess, resolvePath } from "@opencode/core/file-access"
import { FileSystem } from "@opencode/core/filesystem"
import { fileDiff } from "@opencode/core/tool/plugin/file-diff"
import { Location } from "@opencode/core/location"
import { Mime } from "@opencode/core/mime"
import { Permission } from "@opencode/core/permission"
import { AbsolutePath, RelativePath } from "@opencode/core/schema"
import { Session } from "@opencode/core/session"
import { SessionMessage } from "@opencode/core/session/message"
import { Tool } from "@opencode/core/tool"
import { Bom } from "@opencode/util/bom"
import { LayerNode } from "@opencode/util/effect/layer-node"
import { FSUtil } from "@opencode/util/fs-util"
import { location } from "./fixture/location"
import { permissionLayer } from "./lib/permission"

const invocation = {
  agent: Agent.ID.make("build"),
  messageID: SessionMessage.ID.make("msg_fs_corpus"),
  sessionID: Session.ID.make("ses_fs_corpus"),
  id: Tool.CallID.make("call-fs-corpus"),
}

const area = process.argv[2]
if (area === "filesystem" || area === "access") {
  console.log = console.error
  console.info = console.error
  console.debug = console.error
}
if (area === "paths") exportPaths()
else if (area === "mime") exportMime()
else if (area === "bom") exportBom()
else if (area === "diff") exportDiff()
else if (area === "filesystem") await exportFilesystem()
else if (area === "access") await exportAccess()
else {
  console.error(`corpus: unknown area ${area ?? ""}`)
  process.exit(1)
}

function exportPaths() {
  const root = "/repo"
  const home = "/home/agent"
  const directory = `${root}/packages/app`
  const contains = (parent: string, child: string) => FSUtil.contains(parent, child)
  const cases = [
    { name: "contains-same", kind: "contains", parent: root, child: root },
    { name: "contains-child", kind: "contains", parent: root, child: `${root}/src/app.ts` },
    { name: "contains-parent-trailing-separator", kind: "contains", parent: `${root}/`, child: `${root}/src` },
    { name: "contains-relative-child", kind: "contains", parent: root, child: "src/app.ts" },
    { name: "contains-sibling", kind: "contains", parent: `${root}/src`, child: `${root}/test` },
    { name: "contains-parent", kind: "contains", parent: `${root}/src`, child: root },
    { name: "contains-escape", kind: "contains", parent: `${root}/src`, child: `${root}/src/../test` },
    { name: "contains-lookalike", kind: "contains", parent: `${root}/src`, child: `${root}/src2/app.ts` },
  ].map((item) => ({ ...item, expected: contains(item.parent, item.child) }))

  const resolveCases = [
    { name: "resolve-dots", path: `${root}/./packages/../packages/app` },
    { name: "resolve-empty-segments", path: `${root}//packages///app` },
    { name: "resolve-root", path: "/" },
    { name: "resolve-missing", path: `${root}/missing-opencode-rs/path` },
    { name: "resolve-msys-drive-looking", path: "/c/Users/me/project" },
    { name: "resolve-wsl-drive-looking", path: "/mnt/c/Users/me/project" },
    { name: "resolve-cygdrive-looking", path: "/cygdrive/d/work" },
  ].map((item) => ({ ...item, kind: "resolve", expected: FSUtil.resolve(item.path) }))

  const resolvePathCases = [
    { name: "resolve-path-relative", input: "src/app.ts" },
    { name: "resolve-path-parent", input: "../shared/app.ts" },
    { name: "resolve-path-absolute", input: `${root}/absolute.ts` },
    { name: "resolve-path-home", input: "~/notes.md" },
    { name: "resolve-path-home-only", input: "~" },
    { name: "resolve-path-home-nested", input: "~/projects/app/./src" },
    { name: "resolve-path-msys", input: "/c/Users/me/project" },
    { name: "resolve-path-wsl", input: "/mnt/c/Users/me/project" },
    { name: "resolve-path-cygdrive", input: "/cygdrive/d/work" },
  ].map((item) => ({
    ...item,
    kind: "resolvePath",
    directory,
    home,
    expected: resolvePath(directory, item.input, home),
  }))

  write([...cases, ...resolveCases, ...resolvePathCases], root, home)
}

function exportMime() {
  const cases: Array<Record<string, unknown>> = []
  const media = (name: string, input: Uint8Array) => {
    const detected = Mime.detect(input)
    cases.push({
      name: `detect-${name}`,
      kind: "detect",
      bytes: Buffer.from(input).toString("base64"),
      expected: detected,
    })
    cases.push({
      name: `media-${name}`,
      kind: "media",
      bytes: Buffer.from(input).toString("base64"),
      expected: Mime.detectMedia(input) ?? null,
    })
  }
  const detect = (name: string, input: Uint8Array) =>
    cases.push({
      name: `detect-${name}`,
      kind: "detect",
      bytes: Buffer.from(input).toString("base64"),
      expected: Mime.detect(input),
    })

  detect("empty", Uint8Array.of())
  detect("ascii", Uint8Array.of(65))
  detect("nul", Uint8Array.of(0))
  detect("high-byte", Uint8Array.of(0xff))
  detect("truncated-utf8", Uint8Array.of(0xe2, 0x82))
  detect("controls-at-limit", Uint8Array.of(1, 2, 3, 97, 98, 99, 100, 101, 102, 103))
  detect("controls-over-limit", Uint8Array.of(1, 2, 3, 4, 97, 98, 99, 100, 101, 102))
  detect("allowed-controls", Uint8Array.of(9, 10, 13))
  detect("bm-text", Buffer.from("BM"))
  detect("bmw-text", Buffer.from("BMW service schedule\n"))

  const windowsBmp = "424d3a000000000000003600000028000000010000000100000001001800" + "00".repeat(28)
  const os2Bmp = "424d1e000000000000001a0000000c000000010001000100180000000000"
  media("bmp-windows", Buffer.from(windowsBmp, "hex"))
  media("bmp-windows-prefix", Buffer.from(windowsBmp, "hex").subarray(0, 18))
  media("bmp-os2", Buffer.from(os2Bmp, "hex"))
  media("bmp-os2-prefix", Buffer.from(os2Bmp, "hex").subarray(0, 18))

  media("png", Buffer.from("89504e470d0a1a0a0000000d49484452", "hex"))
  media("jpeg", Buffer.from("ffd8ffe000104a464946", "hex"))
  media("gif87a", Buffer.from("GIF87a", "ascii"))
  media("gif89a", Buffer.from("GIF89a", "ascii"))
  media("pdf", Buffer.from("%PDF-1.7\n", "ascii"))
  media("webp", Buffer.from("52494646180000005745425056503820", "hex"))
  media("riff-not-webp", Buffer.from("52494646180000005741564556503820", "hex"))

  for (const [name, value] of [
    ["avif", "000000206674797061766966000000004d695072617669666d6961666d696631"],
    ["avis", "000000186674797061766973000000006d73663161766973"],
    ["mif1-avif", "00000018667479706d69663100000000617669666d69663100000000"],
    ["heic", "000000246674797068656963000000006d6966314d6950726d6961664d694842"],
    ["mif1-heic", "00000018667479706d69663100000000686569636d69663100000000"],
    ["mp4", "000000206674797069736f6d0000020069736f6d69736f32617663316d703431"],
    ["quicktime", "0000001466747970717420200000020071742020200000000877696465"],
    ["m4a", "0000001c667479704d344120000002004d34412069736f6d69736f3200000008"],
  ] as const)
    media(`ftyp-${name}`, Buffer.from(value, "hex"))

  for (const [name, mime] of [
    ["empty", ""],
    ["text", "text/plain"],
    ["json", "application/json"],
    ["json-suffix", "application/ld+json"],
    ["xml", "application/xml"],
    ["xml-suffix", "image/svg+xml"],
    ["javascript", "application/javascript"],
    ["x-javascript", "application/x-javascript"],
    ["binary", "application/octet-stream"],
    ["image", "image/png"],
  ] as const)
    cases.push({ name: `textual-${name}`, kind: "textual", mime, expected: Mime.isTextual(mime) })

  for (const path of [
    "",
    "file.png",
    "photo.JPEG",
    "archive.tar.gz",
    "index.html",
    "style.css",
    "unknown.ext",
    "no-extension",
    ".gitignore",
    "dir/file.",
    "dir/",
    "C:\\tmp\\photo.PNG",
  ])
    cases.push({ name: `lookup-${path || "empty"}`, kind: "lookup", path, expected: FSUtil.mimeType(path) })

  write(cases, "/repo", "/home/agent")
}

function exportBom() {
  const text = "a\uFEFF\u00e9"
  const cases: Array<Record<string, unknown>> = []
  for (const row of [
    { prefix: "", bom: false },
    { prefix: "", bom: true },
    { prefix: "\uFEFF", bom: false },
    { prefix: "\uFEFF", bom: true },
    { prefix: "\uFEFF\uFEFF", bom: false },
    { prefix: "\uFEFF\uFEFF", bom: true },
  ]) {
    const input = Buffer.from(row.prefix + text)
    const synced = Bom.syncBytes(input, row.bom)
    cases.push({
      name: `sync-${JSON.stringify(row.prefix)}-${row.bom}`,
      kind: "sync",
      input: input.toString("base64"),
      bom: row.bom,
      text: synced.text,
      bytes: synced.bytes === undefined ? null : Buffer.from(synced.bytes).toString("base64"),
    })
  }
  for (const value of ["", "\uFEFF", "\uFEFF\uFEFFx", "a\uFEFF", "plain", "\uFEFF\uFEFF"]) {
    const split = Bom.split(value)
    cases.push({
      name: `split-${JSON.stringify(value)}`,
      kind: "split",
      text: value,
      bom: split.bom,
      expected: split.text,
    })
  }
  for (const value of ["", "\uFEFF", "\uFEFF\uFEFFx", "a\uFEFF", "plain"]) {
    for (const bom of [false, true])
      cases.push({
        name: `join-${JSON.stringify(value)}-${bom}`,
        kind: "join",
        text: value,
        bom,
        expected: Bom.join(value, bom),
      })
  }
  write(cases, "/repo", "/home/agent")
}

function exportDiff() {
  const cases: Array<Record<string, unknown>> = []
  const add = (
    name: string,
    before: string,
    after: string,
    file = "file.txt",
    status: "added" | "deleted" | "modified" = "modified",
  ) => {
    const diff = fileDiff(file, before, after, status)
    cases.push({
      name,
      file,
      before,
      after,
      status,
      patch: diff.patch,
      twoFilesPatch: createTwoFilesPatch("old.txt", "new.txt", before, after),
      trimmed: trimDiff(diff.patch),
      additions: diff.additions,
      deletions: diff.deletions,
      changes: diffLines(before, after).map((item) => ({
        value: item.value,
        count: item.count ?? 0,
        added: item.added ?? false,
        removed: item.removed ?? false,
      })),
    })
  }
  const rows = (count: number, prefix = "line") => Array.from({ length: count }, (_, index) => `${prefix} ${index + 1}`)
  const text = (lines: string[]) => lines.join("\n") + "\n"

  add("empty-empty", "", "")
  add("empty-to-line", "", "a\n")
  add("empty-to-no-newline", "", "a")
  add("line-to-empty", "a\n", "")
  add("line-to-empty-no-newline", "a", "")
  add("identical", "a\nb\n", "a\nb\n")
  add("identical-no-newline", "a\nb", "a\nb")
  add("identical-blank-lines", "\n\n\n", "\n\n\n")
  add("identical-empty-line", "\n", "\n")
  add("append-line", "a\n", "a\nb\n")
  add("append-no-final-newline", "a", "a\nb")
  add("append-to-no-final", "a\nb", "a\nb\nc")
  add("remove-tail", "a\nb\n", "a\n")
  add("remove-tail-no-final", "a\nb", "a")
  add("replace-last", "a\nb\n", "a\nc\n")
  add("replace-first", "a\nb\n", "c\nb\n")
  add("replace-middle", "a\nb\nc\n", "a\nx\nc\n")
  add("insert-middle", "a\nc\n", "a\nb\nc\n")
  add("insert-two-middle", "a\nd\n", "a\nb\nc\nd\n")
  add("delete-middle", "a\nb\nc\n", "a\nc\n")
  add("delete-two-middle", "a\nb\nc\nd\n", "a\nd\n")
  add("insert-at-start", "b\n", "a\nb\n")
  add("insert-at-start-no-final", "b", "a\nb")
  add("delete-first", "a\nb\n", "b\n")
  add(
    "two-changes-close",
    text(rows(20)),
    text(rows(20).map((line, index) => (index === 4 || index === 12 ? `${line} changed` : line))),
  )
  add(
    "two-changes-far",
    text(rows(30)),
    text(rows(30).map((line, index) => (index === 2 || index === 22 ? `${line} changed` : line))),
  )
  add(
    "three-hunks-long",
    text(rows(40)),
    text(rows(40).map((line, index) => (index === 4 || index === 19 || index === 34 ? `${line} changed` : line))),
  )
  add(
    "change-at-start-and-end",
    text(rows(15)),
    text(rows(15).map((line, index) => (index === 0 || index === 14 ? `${line} changed` : line))),
  )
  add("long-append", text(rows(30)), text([...rows(30), ...rows(5, "extra")]))
  add("long-delete-tail", text(rows(30)), text(rows(25)))
  add("long-rewrite", text(rows(20)), text(rows(20, "rewritten")))
  add("crlf-identical", "a\r\nb\r\n", "a\r\nb\r\n")
  add("crlf-append", "a\r\n", "a\r\nb\r\n")
  add("crlf-edit", "a\r\nb\r\nc\r\n", "a\r\nx\r\nc\r\n")
  add("crlf-no-final", "a\r\nb", "a\r\nc")
  add("crlf-mixed", "a\r\nb\nc\r\n", "a\r\nx\nc\r\n")
  add("crlf-to-lf", "a\r\nb\r\n", "a\nb\n")
  add("lone-cr", "a\rb\r", "a\rc\r")
  add("add-newline-at-eof", "a\nb", "a\nb\n")
  add("remove-newline-at-eof", "a\nb\n", "a\nb")
  add("no-newline-mid-change", "a\nb", "a\nc")
  add("empty-to-blank-line", "", "\n")
  add("blank-line-to-empty", "\n", "")
  add("repeated-lines", "x\ny\nx\ny\nx\n", "x\ny\nx\nz\nx\n")
  add("repeated-lines-shift", "a\na\na\nb\n", "a\na\nb\na\n")
  add("moved-block", "a\nb\nc\nd\ne\nf\n", "a\ne\nf\nb\nc\nd\n")
  add("swap-adjacent", "a\nb\nc\nd\n", "a\nc\nb\nd\n")
  add("alternating", "a\nb\na\nb\na\nb\n", "b\na\nb\na\nb\na\n")
  add("unicode", "héllo\n世界\n", "héllo\nwörld\n")
  add("emoji", "one\n🙂\nthree\n", "one\n🙃\nthree\n")
  add("whitespace-only-lines", "a\n   \nb\n", "a\n\t\nb\n")
  add("indented", "function {\n  a\n  b\n}\n", "function {\n  a\n  c\n}\n")
  add("tabs", "if {\n\ta\n\tb\n}\n", "if {\n\ta\n\tc\n}\n")
  add("uniform-indent", "  a\n  b\n", "  a\n  c\n")
  add("nonuniform-indent", "    a\n  b\n", "    a\n  c\n")
  add("indent-with-blank-line", "  a\n\n  b\n", "  a\n\n  c\n")
  add("no-indent", "a\nb\n", "a\nc\n")
  add("trailing-spaces", "a  \nb\n", "a\nb\n")
  add("status-added", "", "new\n", "new.txt", "added")
  add("status-deleted", "old\n", "", "old.txt", "deleted")
  add("status-modified", "one\n", "two\n", "file.txt", "modified")
  add("one-line-change", "hello", "world")
  add("one-line-identical", "hello", "hello")

  write(cases, "/repo", "/home/agent")
}

function trimDiff(diff: string) {
  const lines = diff.split("\n")
  const content = lines.filter(
    (line) =>
      (line.startsWith("+") || line.startsWith("-") || line.startsWith(" ")) &&
      !line.startsWith("---") &&
      !line.startsWith("+++"),
  )
  if (content.length === 0) return diff
  const indent = content.reduce((result, line) => {
    const value = line.slice(1)
    if (value.trim().length === 0) return result
    return Math.min(result, value.match(/^(\s*)/)?.[1].length ?? result)
  }, Infinity)
  if (indent === Infinity || indent === 0) return diff
  return lines
    .map((line) => {
      if (
        (line.startsWith("+") || line.startsWith("-") || line.startsWith(" ")) &&
        !line.startsWith("---") &&
        !line.startsWith("+++")
      ) {
        return line[0] + line.slice(1 + indent)
      }
      return line
    })
    .join("\n")
}

async function exportFilesystem() {
  const base = await fs.mkdtemp(path.join(os.tmpdir(), "opencode-fs-corpus-"))
  const specs: Array<Record<string, any>> = [
    {
      name: "read-text",
      kind: "read",
      location: ".",
      fixture: { files: [{ path: "text.txt", text: "hello" }] },
      input: "text.txt",
    },
    {
      name: "read-binary",
      kind: "read",
      location: ".",
      fixture: { files: [{ path: "data.bin", base64: "AAECAwQ=" }] },
      input: "data.bin",
    },
    {
      name: "read-range",
      kind: "read",
      location: ".",
      fixture: { files: [{ path: "data.bin", base64: "AAECAwQ=" }] },
      input: "data.bin",
      range: { offset: 1, bytesToRead: 3 },
    },
    {
      name: "read-lexical-escape",
      kind: "read",
      location: "current",
      fixture: { dirs: ["current"], files: [{ path: "outside.txt", text: "outside" }] },
      input: "../outside.txt",
    },
    {
      name: "read-nul",
      kind: "read",
      location: ".",
      fixture: { files: [{ path: "text.txt", text: "hello" }] },
      input: "te\u0000xt",
    },
    {
      name: "read-symlink-escape",
      kind: "read",
      location: "current",
      fixture: {
        dirs: ["current", "outside"],
        files: [{ path: "outside/file.txt", text: "outside" }],
        symlinks: [{ path: "current/link", target: "../outside" }],
      },
      input: "link/file.txt",
    },
    { name: "read-missing", kind: "read", location: ".", input: "missing.txt" },
    { name: "read-directory", kind: "read", location: ".", fixture: { dirs: ["src"] }, input: "src" },
    {
      name: "list-direct-children",
      kind: "list",
      location: ".",
      fixture: { dirs: ["src"], files: [{ path: "README.md", text: "# Test" }] },
    },
    {
      name: "list-sorted",
      kind: "list",
      location: ".",
      fixture: {
        dirs: ["z-dir", "a-dir"],
        files: [
          { path: "b.txt", text: "b" },
          { path: "a.txt", text: "a" },
          { path: "c.json", text: "{}" },
        ],
      },
      input: ".",
    },
    {
      name: "list-parent",
      kind: "list",
      location: "current",
      fixture: { dirs: ["current", "sibling"], files: [{ path: "sibling/file.txt", text: "outside" }] },
      input: "..",
    },
    {
      name: "list-sibling",
      kind: "list",
      location: "current",
      fixture: { dirs: ["current", "sibling"], files: [{ path: "sibling/file.txt", text: "outside" }] },
      input: "../sibling",
    },
    {
      name: "list-absolute",
      kind: "list",
      location: "current",
      fixture: { dirs: ["current", "sibling"], files: [{ path: "sibling/file.txt", text: "outside" }] },
      absoluteInput: "sibling",
    },
    {
      name: "list-symlink",
      kind: "list",
      location: "current",
      fixture: {
        dirs: ["current", "outside"],
        files: [{ path: "outside/file.txt", text: "outside" }],
        symlinks: [{ path: "current/link", target: "../outside" }],
      },
      input: "link",
    },
    {
      name: "list-file",
      kind: "list",
      location: ".",
      fixture: { files: [{ path: "text.txt", text: "hello" }] },
      input: "text.txt",
    },
    { name: "list-missing", kind: "list", location: ".", input: "missing" },
    {
      name: "write-symlink",
      kind: "write",
      location: "current",
      fixture: { dirs: ["current", "outside"], symlinks: [{ path: "current/link", target: "../outside" }] },
      input: "link/new.txt",
      base64: Buffer.from("created").toString("base64"),
    },
  ]
  const cases: Array<Record<string, unknown>> = []
  for (const spec of specs) {
    const directory = path.join(base, spec.name)
    await fs.mkdir(directory, { recursive: true })
    await materialize(directory, spec.fixture ?? {})
    const locationDirectory = path.join(directory, spec.location ?? ".")
    const input = spec.absoluteInput ? path.join(directory, spec.absoluteInput) : spec.input
    const caseSpec = { ...spec, input }
    const exit = await Effect.runPromiseExit(
      filesystemProgram(caseSpec).pipe(Effect.provide(filesystemProvider(locationDirectory))),
    )
    cases.push({
      ...caseSpec,
      expected: Exit.isSuccess(exit) ? exit.value : { error: errorTag(exit) },
    })
  }
  write(cases, base, os.homedir())
  await fs.rm(base, { recursive: true, force: true })
}

function filesystemProvider(directory: string) {
  return LayerNode.compile(FileSystem.node, {
    replacements: [
      Location.node.replace(
        Layer.succeed(
          Location.Service,
          Location.Service.of(location({ directory: AbsolutePath.make(directory) })),
        ),
      ),
    ],
  })
}

function filesystemProgram(spec: Record<string, any>) {
  if (spec.kind === "read") {
    return Effect.gen(function* () {
      const service = yield* FileSystem.Service
      const file = yield* service.read({ path: RelativePath.make(spec.input) })
      const bytes = yield* Stream.mkUint8Array(file.stream(spec.range))
      return {
        mime: file.mime,
        size: file.size,
        mtime: Option.isSome(file.mtime),
        bytes: Buffer.from(bytes).toString("base64"),
      }
    })
  }
  if (spec.kind === "list") {
    return Effect.gen(function* () {
      const service = yield* FileSystem.Service
      const entries = yield* service.list(spec.input === undefined ? undefined : { path: RelativePath.make(spec.input) })
      return entries.map((entry) => ({ path: entry.path, type: entry.type }))
    })
  }
  if (spec.kind === "write") {
    return Effect.gen(function* () {
      const service = yield* FileSystem.Service
      const written = yield* service.write({ path: spec.input, data: Buffer.from(spec.base64, "base64") })
      const bytes = yield* Effect.promise(() => fs.readFile(written.path))
      return { path: written.path, bytes: Buffer.from(bytes).toString("base64") }
    })
  }
  throw new Error(`corpus: unknown filesystem case ${spec.kind}`)
}

async function exportAccess() {
  const base = await fs.mkdtemp(path.join(os.tmpdir(), "opencode-fs-corpus-"))
  const specs: Array<Record<string, any>> = [
    {
      name: "resolve-internal-relative",
      location: ".",
      fixture: { files: [{ path: "hello.txt", text: "hello" }] },
      ops: [{ op: "resolve", path: "hello.txt" }],
    },
    {
      name: "resolve-internal-prospective",
      location: ".",
      fixture: { dirs: ["src"] },
      ops: [{ op: "resolve", path: "src/new.txt" }],
    },
    {
      name: "resolve-relative-escape",
      location: "current",
      fixture: { dirs: ["current"] },
      ops: [{ op: "resolve", path: "../outside.txt" }],
    },
    {
      name: "resolve-project-worktree",
      location: "packages/opencode",
      project: ".",
      fixture: { dirs: ["packages/opencode"] },
      ops: [{ op: "resolve", path: "../../README.md" }],
    },
    {
      name: "resolve-root-sentinel",
      location: "current",
      project: "/",
      fixture: { dirs: ["current"] },
      ops: [{ op: "resolve", path: "../outside.txt" }],
    },
    {
      name: "resolve-absolute-internal",
      location: ".",
      ops: [{ op: "resolve", absoluteInput: "new.txt" }],
    },
    {
      name: "resolve-absolute-external",
      location: "current",
      fixture: { dirs: ["current"] },
      ops: [{ op: "resolve", absoluteInput: "outside/new.txt" }],
    },
    {
      name: "resolve-existing-external-file",
      location: "current",
      fixture: { dirs: ["current", "outside"], files: [{ path: "outside/existing.txt", text: "existing" }] },
      ops: [{ op: "resolve", absoluteInput: "outside/existing.txt" }],
    },
    {
      name: "resolve-explicit-file-kind",
      location: "current",
      fixture: { dirs: ["current", "outside"] },
      ops: [{ op: "resolve", absoluteInput: "outside", kind: "file" }],
    },
    {
      name: "resolve-external-descendant",
      location: "current",
      fixture: { dirs: ["current"] },
      ops: [{ op: "resolve", absoluteInput: "outside/new/nested/file.txt" }],
    },
    {
      name: "resolve-repo-save",
      location: "current",
      fixture: { dirs: ["current", "outside/.git", "outside/src"] },
      ops: [{ op: "resolve", absoluteInput: "outside/src" }],
    },
    {
      name: "resolve-symlink-lexical",
      location: "current",
      fixture: { dirs: ["current", "outside"], symlinks: [{ path: "current/escape", target: "../outside" }] },
      ops: [{ op: "resolve", path: "escape/new.txt" }],
    },
    {
      name: "resolve-in-location-symlink",
      location: ".",
      fixture: { dirs: ["actual"], symlinks: [{ path: "linked", target: "actual" }] },
      ops: [{ op: "resolve", path: "linked/new.txt" }],
    },
    {
      name: "authorize-read-internal",
      location: ".",
      fixture: { files: [{ path: "README.md", text: "# Test" }] },
      ops: [{ op: "authorizeRead", path: "src/../README.md" }],
    },
    {
      name: "authorize-read-external",
      location: "current",
      fixture: { dirs: ["current"] },
      ops: [{ op: "authorizeRead", path: "../notes.txt" }],
    },
    {
      name: "authorize-read-sibling",
      location: "current",
      fixture: { dirs: ["current"] },
      ops: [
        { op: "authorizeRead", path: "../report final.txt" },
        { op: "authorizeRead", path: "../report\u202ffinal.txt", siblingOf: 0 },
        { op: "authorizeRead", path: "../notes.txt" },
      ],
    },
    {
      name: "authorize-read-not-sibling",
      location: ".",
      fixture: { files: [{ path: "README.md", text: "# Test" }] },
      ops: [
        { op: "authorizeRead", path: "README.md" },
        { op: "authorizeRead", path: "../notes.txt", siblingOf: 0 },
      ],
    },
    {
      name: "authorize-denied-external",
      location: "current",
      fixture: { dirs: ["current"] },
      deny: "external_directory",
      ops: [{ op: "authorizeRead", path: "../notes.txt" }],
    },
    {
      name: "authorize-denied-read",
      location: "current",
      fixture: { dirs: ["current"] },
      deny: "read",
      ops: [{ op: "authorizeRead", path: "../notes.txt" }],
    },
    {
      name: "authorize-external-batch",
      location: "current",
      fixture: {
        dirs: ["current", "git/.git", "git/nested", "hg/.hg", "hg/nested"],
        files: [{ path: "current/README.md", text: "# Test" }],
      },
      metadata: (directory: string) => ({
        filepath: path.join(directory, "git", "nested", "a.txt"),
        parentDir: path.join(directory, "git", "nested"),
      }),
      ops: [
        { op: "resolve", absoluteInput: "git/nested/a.txt", kind: "file" },
        { op: "resolve", absoluteInput: "git/nested/b.txt", kind: "file" },
        { op: "resolve", absoluteInput: "hg/nested/c.txt", kind: "file" },
        { op: "resolve", path: "README.md" },
        { op: "authorizeExternal", targets: [0, 3, 1, 2, 0], metadata: true },
        { op: "authorizeExternal", targets: [3] },
        { op: "authorizeExternal", targets: [1] },
      ],
    },
  ]
  const cases: Array<Record<string, unknown>> = []
  for (const spec of specs) {
    const directory = path.join(base, spec.name)
    await fs.mkdir(directory, { recursive: true })
    await materialize(directory, spec.fixture ?? {})
    const locationDirectory = path.join(directory, spec.location ?? ".")
    const projectDirectory =
      spec.project === "/" ? "/" : spec.project === undefined ? locationDirectory : path.join(directory, spec.project)
    const caseMetadata = typeof spec.metadata === "function" ? spec.metadata(directory) : spec.metadata
    const requests: Array<Record<string, unknown>> = []
    const targets: Array<FileAccess.Target> = []
    const results: Array<unknown> = []
    const program = Effect.gen(function* () {
      const access = yield* FileAccess.Service
      for (const op of spec.ops) {
        const input = op.absoluteInput ? path.join(directory, op.absoluteInput) : op.path
        if (op.op === "resolve") {
          const target = yield* access.resolve({ path: input, kind: op.kind })
          targets.push(target)
          results.push(targetValue(target))
        } else if (op.op === "authorizeExternal") {
          const metadata = op.metadata === true ? caseMetadata : op.metadata
          yield* access.authorizeExternal(
            op.targets.map((index: number) => targets[index]),
            invocation,
            metadata,
          )
          results.push(null)
        } else if (op.op === "authorizeRead") {
          const options = op.siblingOf === undefined ? undefined : { siblingOf: targets[op.siblingOf] }
          const target = yield* access.authorizeRead(input, invocation, options)
          targets.push(target)
          results.push(targetValue(target))
        }
      }
    })
    const exit = await Effect.runPromiseExit(
      program.pipe(Effect.provide(accessProvider(locationDirectory, projectDirectory, requests, spec.deny))),
    )
    const expected: Record<string, unknown> = {
      targets: results.filter((value) => value !== null),
      requests,
    }
    if (Exit.isFailure(exit)) expected.error = errorTag(exit)
    cases.push({ ...spec, metadata: caseMetadata, expected })
  }
  write(cases, base, os.homedir())
  await fs.rm(base, { recursive: true, force: true })
}

function accessProvider(
  directory: string,
  projectDirectory: string,
  requests: Array<Record<string, unknown>>,
  deny?: string,
) {
  return LayerNode.compile(FileAccess.node, {
    replacements: [
      Permission.node.replace(
        permissionLayer({
          assert: (input) =>
            Effect.gen(function* () {
              requests.push(requestValue(input))
              if (input.action === deny)
                yield* new Permission.BlockedError({
                  rules: [],
                  permission: input.action,
                  resources: input.resources,
                })
            }),
        }),
      ),
      Location.node.replace(
        Layer.succeed(
          Location.Service,
          Location.Service.of(
            location(
              { directory: AbsolutePath.make(directory) },
              { projectDirectory: AbsolutePath.make(projectDirectory) },
            ),
          ),
        ),
      ),
    ],
  })
}

function requestValue(input: Permission.AssertInput) {
  return {
    action: input.action,
    resources: input.resources,
    save: input.save,
    sessionID: input.sessionID,
    agent: input.agent,
    source: input.source,
    ...(input.metadata === undefined ? {} : { metadata: JSON.parse(JSON.stringify(input.metadata)) }),
  }
}

function targetValue(target: FileAccess.Target) {
  return {
    absolute: target.absolute,
    resource: target.resource,
    externalDirectory: target.externalDirectory
      ? {
          action: target.externalDirectory.action,
          directory: target.externalDirectory.directory,
          resource: target.externalDirectory.resource,
          save: target.externalDirectory.save,
        }
      : null,
  }
}

async function materialize(directory: string, fixture: Record<string, any>) {
  for (const dir of fixture.dirs ?? []) await fs.mkdir(path.join(directory, dir), { recursive: true })
  for (const file of fixture.files ?? []) {
    const target = path.join(directory, file.path)
    await fs.mkdir(path.dirname(target), { recursive: true })
    await fs.writeFile(target, file.base64 ? Buffer.from(file.base64, "base64") : (file.text ?? ""))
  }
  for (const link of fixture.symlinks ?? []) {
    const target = path.join(directory, link.path)
    await fs.mkdir(path.dirname(target), { recursive: true })
    await fs.symlink(link.target, target)
  }
}

function errorTag(exit: Exit.Exit<unknown, unknown>) {
  if (Exit.isSuccess(exit)) return undefined
  const found = Cause.findErrorOption(exit.cause)
  if (Option.isSome(found) && typeof found.value === "object" && found.value !== null && "_tag" in found.value)
    return String((found.value as { _tag: unknown })._tag)
  return "Die"
}

function write(cases: Array<Record<string, unknown>>, root: string, home: string) {
  const output = JSON.stringify(cases, null, 2).replaceAll(root, "{{root}}").replaceAll(home, "{{home}}")
  process.stdout.write(`${output}\n`)
}
