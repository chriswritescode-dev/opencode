import { ConfigMarkdown } from "@opencode/core/config/markdown"

const area = process.argv[2]
if (area === "markdown") exportMarkdown()
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
