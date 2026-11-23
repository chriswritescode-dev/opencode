import { ConfigExperimental } from "@opencode/schema/config/experimental"
import { Document, Info } from "@opencode/schema/config"
import { FlagResolve } from "@opencode/core/flag/resolve"

const area = process.argv[2]
if (area !== "flags") {
  console.error(`corpus: unknown area ${area ?? ""}`)
  process.exit(1)
}

const document = (experimental?: ConstructorParameters<typeof ConfigExperimental.Info>[0]) =>
  new Document({
    type: "document",
    info: new Info(experimental === undefined ? {} : { experimental: new ConfigExperimental.Info(experimental) }),
  })

const cases: Record<string, { entries: Array<Document>; channel: string; overrides?: string }> = {
  "default-beta": { entries: [], channel: "beta" },
  "default-dev": { entries: [], channel: "dev" },
  "default-latest": { entries: [], channel: "latest" },
  "default-local": { entries: [], channel: "local" },
  "default-my-branch": { entries: [], channel: "my-branch" },
  "default-unknown": { entries: [], channel: "unknown" },
  "opt-in-wildcard": { entries: [], channel: "dev", overrides: "all" },
  "opt-in-env": { entries: [], channel: "latest", overrides: "todo" },
  "opt-in-config": { entries: [document({ todo: true })], channel: "latest" },
  "per-flag-env-name": { entries: [], channel: "latest", overrides: "full_codemode" },
  "per-flag-config-true": { entries: [document({ full_codemode: true })], channel: "latest" },
  "per-flag-config-false": { entries: [document({ full_codemode: false })], channel: "dev" },
  "per-flag-negative-override": { entries: [], channel: "dev", overrides: "-full_codemode" },
  "all-wildcard-beats-config-false": {
    entries: [document({ full_codemode: false })],
    channel: "latest",
    overrides: "all",
  },
  "all-flags-true": { entries: [document({ all_flags: true })], channel: "latest" },
  precedence: {
    entries: [document({ full_codemode: true, video_generate: false })],
    channel: "dev",
    overrides: " media_blob , -full_codemode,unknown_flag",
  },
  "settings-object-turns-flag-on": { entries: [document({ video_generate: {} })], channel: "latest" },
  "highest-priority-document-sets-experimental": {
    entries: [document({ full_codemode: true }), document({ subagent_depth: 2 }), document()],
    channel: "latest",
  },
  "all-wildcard": { entries: [document({ media_blob: false })], channel: "latest", overrides: "all" },
  "all-wildcard-with-negative-overrides": { entries: [], channel: "latest", overrides: " -media_blob , all, " },
  "disabled-all-wildcard-falls-through": {
    entries: [document({ media_blob: false })],
    channel: "latest",
    overrides: "-all",
  },
  "all-flags-below-per-flag-and-overrides": {
    entries: [document({ all_flags: true, media_blob: false })],
    channel: "latest",
    overrides: "-artifacts",
  },
  "all-flags-false-is-not-a-kill-switch": { entries: [document({ all_flags: false })], channel: "dev" },
  "all-flags-in-overrides-is-not-a-wildcard": { entries: [], channel: "latest", overrides: "all_flags" },
  "all-flags-survives-project-experimental": {
    entries: [document({ all_flags: true }), document({ subagent_depth: 2, full_codemode: false })],
    channel: "latest",
  },
  "project-all-flags-false-stops-global": {
    entries: [document({ all_flags: true }), document({ all_flags: false })],
    channel: "latest",
  },
}

const lines = Object.keys(cases)
  .sort()
  .map((name) => {
    const input = cases[name]!
    return `${JSON.stringify(name)}:${JSON.stringify({
      entries: JSON.parse(JSON.stringify(input.entries)),
      channel: input.channel,
      overrides: input.overrides ?? null,
      expected: FlagResolve.resolve(input),
    })}`
  })
process.stdout.write(`{\n${lines.join(",\n")}\n}\n`)
