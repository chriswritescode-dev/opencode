import { VcsPatch } from "@opencode/core/vcs/patch"

const empty = [
  ["empty-plain-path", "src/index.ts"],
  ["empty-path-with-space", "src/my file.ts"],
  ["empty-non-ascii-path", "src/naïve/文件.ts"],
  ["empty-path-with-tab", "src/with\ttab.ts"],
].map(([name, file]) => ({ name, file, patch: VcsPatch.emptyPatch(file) }))

const contents = [
  ["empty-content", ""],
  ["one-line-trailing-newline", "alpha\n"],
  ["one-line-no-trailing-newline", "alpha"],
  ["several-lines-trailing-newline", "alpha\nbeta\ngamma\n"],
  ["several-lines-no-trailing-newline", "alpha\nbeta\ngamma"],
  ["lone-newline", "\n"],
  ["blank-line-in-middle", "alpha\n\nbeta\n"],
  ["crlf-trailing-newline", "alpha\r\nbeta\r\n"],
  ["crlf-no-trailing-newline", "alpha\r\nbeta"],
  ["non-ascii-content", "héllo\n世界\n"],
  ["lone-carriage-return-suffix", "alpha\r"],
]

const add = contents.map(([name, content]) => ({
  name: `add-${name}`,
  file: "src/added.ts",
  content,
  patch: VcsPatch.addPatch("src/added.ts", content),
}))

const remove = contents.map(([name, content]) => ({
  name: `delete-${name}`,
  file: "src/deleted.ts",
  content,
  patch: VcsPatch.deletePatch("src/deleted.ts", content),
}))

const countPatches: Array<[string, string]> = [
  ["count-added-file-patch", VcsPatch.addPatch("src/count.ts", "one\ntwo\n")],
  ["count-deleted-file-patch", VcsPatch.deletePatch("src/count.ts", "one\ntwo\n")],
  ["count-file-headers-only", "--- a/src/headers.ts\n+++ b/src/headers.ts\n"],
  ["count-added-and-removed-hunk", "--- a/src/hunk.ts\n+++ b/src/hunk.ts\n@@ -1,2 +1,2 @@\n context\n-old\n+new\n"],
  [
    "count-header-like-content-lines",
    "--- a/src/lookalike.ts\n+++ b/src/lookalike.ts\n@@ -1,3 +1,3 @@\n keep\n--- removed content line\n+++ added content line\n+actual addition\n- actual removal\n",
  ],
  ["count-empty-text", ""],
]

const count = countPatches.map(([name, patch]) => ({
  name,
  patch,
  ...VcsPatch.countPatch(patch),
}))

const fileChunks: Array<[string, string]> = [
  [
    "file-full-diff-header-and-file-headers",
    "diff --git a/src/one.ts b/src/one.ts\n--- a/src/one.ts\n+++ b/src/one.ts\n@@ -1 +1 @@\n-a\n+b\n",
  ],
  ["file-only-file-headers", "--- a/src/only.ts\n+++ b/src/only.ts\n@@ -1 +1 @@\n-a\n+b\n"],
  ["file-new-side-dev-null", "--- a/src/old.ts\n+++ /dev/null\n@@ -1 +0,0 @@\n-a\n"],
  ["file-old-side-dev-null", "--- /dev/null\n+++ b/src/new.ts\n@@ -0,0 +1 @@\n+b\n"],
  [
    "file-both-sides-dev-null-with-header",
    "diff --git a/src/ghost.ts b/src/ghost.ts\n--- /dev/null\n+++ /dev/null\n@@ -0,0 +0,0 @@\n",
  ],
  ["file-quoted-tab-escape", '--- "a/src/with\\ttab.ts"\n+++ "b/src/with\\ttab.ts"\n@@ -1 +1 @@\n-a\n+b\n'],
  ["file-quoted-octal-escapes", '--- "a/src/caf\\303\\251.ts"\n+++ "b/src/caf\\303\\251.ts"\n@@ -1 +1 @@\n-a\n+b\n'],
  [
    "file-quoted-diff-header-without-file-headers",
    'diff --git "a/src/one two.ts" "b/src/one two.ts"\n@@ -1 +1 @@\n-a\n+b\n',
  ],
  [
    "file-unquoted-diff-header-with-spaces",
    "diff --git a/src/one two.ts b/src/one two.ts\n@@ -1 +1 @@\n-a\n+b\n",
  ],
  [
    "file-crlf-chunk",
    "diff --git a/src/crlf.ts b/src/crlf.ts\r\n--- a/src/crlf.ts\r\n+++ b/src/crlf.ts\r\n@@ -1 +1 @@\r\n-a\r\n+b\r\n",
  ],
  ["file-no-path-information", "@@ -1 +1 @@\n-a\n+b\n"],
]

const fileFromPatchChunk = fileChunks.map(([name, chunk]) => ({
  name,
  chunk,
  file: VcsPatch.fileFromPatchChunk(chunk) ?? null,
}))

const chunkOne =
  "diff --git a/src/one.ts b/src/one.ts\n--- a/src/one.ts\n+++ b/src/one.ts\n@@ -1 +1 @@\n-a\n+b\n"
const chunkTwo =
  "diff --git a/src/two.ts b/src/two.ts\n--- a/src/two.ts\n+++ b/src/two.ts\n@@ -1 +1 @@\n-a\n+b\n"
const chunkOneCrlf =
  "diff --git a/src/one.ts b/src/one.ts\r\n--- a/src/one.ts\r\n+++ b/src/one.ts\r\n@@ -1 +1 @@\r\n-a\r\n+b\r\n"
const chunkTwoCrlf =
  "diff --git a/src/two.ts b/src/two.ts\r\n--- a/src/two.ts\r\n+++ b/src/two.ts\r\n@@ -1 +1 @@\r\n-a\r\n+b\r\n"

const splitInputs: Array<[string, string, boolean]> = [
  ["split-empty-text", "", false],
  ["split-one-chunk", chunkOne, false],
  ["split-two-chunks-lf", `${chunkOne}${chunkTwo}`, false],
  ["split-two-chunks-crlf", `${chunkOneCrlf}${chunkTwoCrlf}`, false],
  ["split-leading-preamble", `warning: excerpt from git diff\n${chunkOne}${chunkTwo}`, false],
  ["split-truncated-two-chunks", `${chunkOne}${chunkTwo}`, true],
  ["split-truncated-one-chunk", chunkOne, true],
  ["split-diff-not-at-line-start", "prefixdiff --git a/src/one.ts b/src/one.ts\n", false],
]

const splitGitPatch = splitInputs.map(([name, text, truncated]) => ({
  name,
  text,
  truncated,
  chunks: VcsPatch.splitGitPatch({ text, truncated }),
}))

const chunkSameOne =
  "diff --git a/src/same.ts b/src/same.ts\n--- a/src/same.ts\n+++ b/src/same.ts\n@@ -1 +1 @@\n-a\n+b\n"
const chunkSameTwo =
  "diff --git a/src/same.ts b/src/same.ts\n--- a/src/same.ts\n+++ b/src/same.ts\n@@ -3 +3 @@\n-c\n+d\n"
const chunkUnparseableOne = "diff --git first\n@@ -1 +1 @@\n-a\n+b\n"
const chunkUnparseableTwo = "diff --git second\n@@ -1 +1 @@\n-c\n+d\n"

const byFileInputs: Array<[string, string, boolean, Array<string | null>]> = [
  ["chunks-by-file-two-files", `${chunkOne}${chunkTwo}`, false, []],
  ["chunks-by-file-same-file-twice", `${chunkSameOne}${chunkSameTwo}`, false, []],
  [
    "chunks-by-file-dev-null-fallback",
    "diff --git /dev/null\n--- /dev/null\n+++ /dev/null\n@@ -0,0 +1 @@\n+new\n",
    false,
    ["fallback.ts"],
  ],
  ["chunks-by-file-null-fallback-skips", `${chunkUnparseableOne}`, false, [null]],
  [
    "chunks-by-file-truncated-drops-last-before-fallback",
    `${chunkUnparseableOne}${chunkUnparseableTwo}`,
    true,
    ["kept.ts", "dropped.ts"],
  ],
  [
    "chunks-by-file-quoted-path",
    'diff --git "a/src/quote one.ts" "b/src/quote one.ts"\n--- "a/src/quote one.ts"\n+++ "b/src/quote one.ts"\n@@ -1 +1 @@\n-a\n+b\n',
    false,
    [],
  ],
  [
    "chunks-by-file-unquoted-non-ascii",
    "diff --git a/src/naïve/文件.ts b/src/naïve/文件.ts\n--- a/src/naïve/文件.ts\n+++ b/src/naïve/文件.ts\n@@ -1 +1 @@\n-a\n+b\n",
    false,
    [],
  ],
  [
    "chunks-by-file-fallback-index-after-match",
    `${chunkOne}${chunkUnparseableTwo}`,
    false,
    [null, "second.ts"],
  ],
]

const chunksByFile = byFileInputs.map(([name, text, truncated, fallback]) => ({
  name,
  text,
  truncated,
  fallback,
  files: Object.fromEntries(VcsPatch.chunksByFile({ text, truncated }, (index) => fallback[index] ?? undefined)),
}))

console.log(JSON.stringify({ empty, add, delete: remove, count, fileFromPatchChunk, splitGitPatch, chunksByFile }))
