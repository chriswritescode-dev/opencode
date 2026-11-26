import { resolvePath } from "@opencode/core/file-access"
import { Mime } from "@opencode/core/mime"
import { Bom } from "@opencode/util/bom"
import { FSUtil } from "@opencode/util/fs-util"

const area = process.argv[2]
if (area === "paths") exportPaths()
else if (area === "mime") exportMime()
else if (area === "bom") exportBom()
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

function write(cases: Array<Record<string, unknown>>, root: string, home: string) {
  const output = JSON.stringify(cases, null, 2).replaceAll(root, "{{root}}").replaceAll(home, "{{home}}")
  process.stdout.write(`${output}\n`)
}
