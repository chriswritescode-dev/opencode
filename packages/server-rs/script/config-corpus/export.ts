import { ConfigExperimental } from "@opencode/schema/config/experimental"
import { Document, Entry, Event, Info } from "@opencode/schema/config"
import { FlagResolve } from "@opencode/core/flag/resolve"
import { ConfigNormalize } from "@opencode/core/config/normalize"
import { ConfigVariable } from "@opencode/core/config/variable"
import { ConfigDiscovery } from "@opencode/core/config/discovery"
import { ConfigWatch } from "@opencode/core/config/watch"
import { Config } from "@opencode/core/config"
import { ConfigIntegration } from "@opencode/core/config/integration"
import { ConfigManaged } from "@opencode/core/config/managed"
import { ConfigPolicy } from "@opencode/schema/config/policy"
import { App } from "@opencode/core/app"
import { AppNodeBuilder } from "@opencode/core/effect/app-node-builder"
import { makeGlobalNode } from "@opencode/util/effect/app-node"
import { Credential } from "@opencode/core/credential"
import { WellKnown } from "@opencode/core/wellknown"
import { Watcher } from "@opencode/core/filesystem/watcher"
import { Bus } from "@opencode/core/bus"
import { ManagedPolicy } from "@opencode/core/managed-policy"
import { Integration } from "@opencode/schema/integration"
import { FSUtil } from "@opencode/util/fs-util"
import { LayerNode } from "@opencode/util/effect/layer-node"
import { Global } from "@opencode/util/global"
import { Location } from "@opencode/core/location"
import { Project } from "@opencode/core/project"
import { AbsolutePath } from "@opencode/core/schema"
import { Effect, Fiber, Layer, Logger, Option, Schema, Stream } from "effect"
import { parse, type ParseError } from "jsonc-parser"
import path from "node:path"
import { chmod, mkdir, readFile, realpath, rm, stat, symlink, writeFile } from "node:fs/promises"
import { isDeepStrictEqual } from "node:util"

const options = { errors: "all", onExcessProperty: "ignore" } as const
const decodeInfo = Schema.decodeUnknownOption(Info, options)

function insideRoot(root: string, value: string) {
  const relative = path.relative(root, value)
  return relative === "" || (relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative))
}

function relativeToRoot(area: string, name: string, root: string, value: string) {
  if (!insideRoot(root, value)) throw new Error(`corpus: ${area} ${name} produced a path outside its root: ${value}`)
  return path.relative(root, value).split(path.sep).join("/")
}

const area = process.argv[2]
if (area === "flags") exportFlags()
else if (area === "normalize") exportNormalize()
else if (area === "substitute") await exportSubstitute()
else if (area === "discover") await exportDiscover()
else if (area === "load") await exportLoad()
else if (area === "update") await exportUpdate()
else {
  console.error(`corpus: unknown area ${area ?? ""}`)
  process.exit(1)
}

function exportFlags() {
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
    "opencode-api-env-release": { entries: [], channel: "latest", overrides: "opencode_api" },
    "opencode-api-env-local": { entries: [], channel: "local", overrides: "opencode_api" },
    "opencode-api-config-release": { entries: [document({ opencode_api: true })], channel: "latest" },
    "opencode-api-config-local": { entries: [document({ opencode_api: true })], channel: "local" },
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
}

type NormalizeCase = {
  group: "current" | "v1"
  text: string
  throws?: { diagnostic: { kind: string; path: string[]; message: string } }
}

function exportNormalize() {
  const cases: Record<string, NormalizeCase> = {
    "agents-invalid-color": {
      group: "current",
      text: JSON.stringify({ agents: { reviewer: { color: "blue", system: "Keep" } } }),
    },
    "native-agent-permissions-renamed-media-tools": {
      group: "current",
      text: JSON.stringify({
        agents: { reviewer: { permissions: [{ action: "experimental_image_generate", resource: "*", effect: "deny" }] } },
      }),
    },
    "native-permissions-renamed-media-tools": {
      group: "current",
      text: JSON.stringify({
        permissions: [
          { action: "experimental_image_generate", resource: "*", effect: "allow" },
          { action: "experimental_video_generate", resource: "video", effect: "ask" },
        ],
      }),
    },
    "commands-model-string": {
      group: "current",
      text: JSON.stringify({ commands: { review: { template: "review", model: "anthropic/claude#high" } } }),
    },
    "comments-and-trailing-commas": {
      group: "current",
      text: `{ // comment
  "shell": "/bin/bash",
}`,
    },
    "compaction-buffer-hundred": { group: "current", text: JSON.stringify({ compaction: { buffer: "100%" } }) },
    "compaction-buffer-over": { group: "current", text: JSON.stringify({ compaction: { buffer: "150%" } }) },
    "compaction-buffer-percent": { group: "current", text: JSON.stringify({ compaction: { buffer: "12.5%" } }) },
    "compaction-buffer-zero": { group: "current", text: JSON.stringify({ compaction: { buffer: "0%" } }) },
    "compaction-invalid": { group: "current", text: `{"compaction": []}` },
    "compaction-merge": {
      group: "current",
      text: JSON.stringify({
        compaction: {
          auto: false,
          preserve_recent_tokens: 1000,
          keep: { tokens: 2000 },
          reserved: 3000,
          buffer: 4000,
          tail_turns: 2,
          prune: true,
        },
      }),
    },
    "duplicate-keys": { group: "current", text: `{"shell": "/bin/bash", "shell": "/bin/zsh"}` },
    "experimental-all-flags-false": {
      group: "current",
      text: JSON.stringify({ experimental: { all_flags: false } }),
    },
    "experimental-all-flags-invalid": {
      group: "current",
      text: JSON.stringify({ experimental: { all_flags: "yes" } }),
    },
    "experimental-all-flags-true": { group: "current", text: JSON.stringify({ experimental: { all_flags: true } }) },
    "experimental-codemode-vm": {
      group: "current",
      text: JSON.stringify({ experimental: { codemode_vm: true, codemodeVm: false } }),
    },
    "experimental-image-generate-empty": {
      group: "current",
      text: JSON.stringify({ experimental: { image_generate: {} } }),
    },
    "experimental-image-generate-false": {
      group: "current",
      text: JSON.stringify({ experimental: { image_generate: false } }),
    },
    "experimental-image-generate-invalid-model": {
      group: "current",
      text: JSON.stringify({ experimental: { image_generate: { model: "no-slash" } } }),
    },
    "experimental-image-generate-invalid-model-type": {
      group: "current",
      text: JSON.stringify({ experimental: { image_generate: { model: 1 } } }),
    },
    "experimental-image-generate-invalid-number": {
      group: "current",
      text: JSON.stringify({ experimental: { image_generate: 1 } }),
    },
    "experimental-image-generate-invalid-string": {
      group: "current",
      text: JSON.stringify({ experimental: { image_generate: "openai/gpt-image-2" } }),
    },
    "experimental-image-generate-model": {
      group: "current",
      text: JSON.stringify({ experimental: { image_generate: { model: "openai/gpt-image-2#hd" } } }),
    },
    "experimental-image-generate-true": {
      group: "current",
      text: JSON.stringify({ experimental: { image_generate: true } }),
    },
    "experimental-location-timeout-false": {
      group: "current",
      text: JSON.stringify({ experimental: { location_idle_timeout: false } }),
    },
    "experimental-location-timeout-invalid": {
      group: "current",
      text: JSON.stringify({ experimental: { location_idle_timeout: "soon" } }),
    },
    "experimental-video-generate-model": {
      group: "current",
      text: JSON.stringify({ experimental: { video_generate: { model: "openai/sora#hd" } } }),
    },
    "experimental-location-timeout": {
      group: "current",
      text: `{"experimental": {"location_idle_timeout": "2 hours"}}`,
    },
    "experimental-policies-only": {
      group: "current",
      text: JSON.stringify({
        experimental: { policies: [{ action: "provider.use", resource: "*", effect: "deny" }] },
      }),
    },
    "experimental-removed-automations": {
      group: "current",
      text: JSON.stringify({
        automations: false,
        experimental: {
          todo: true,
          media_blob: false,
          artifacts: "yes",
          full_codemode: true,
          automations: false,
          schedules: false,
        },
      }),
    },
    "experimental-subagent-depth": { group: "current", text: `{"experimental": {"subagent_depth": 0}}` },
    "formatter-lsp-boolean": { group: "current", text: `{"formatter": false, "lsp": true}` },
    "formatter-lsp-empty": { group: "current", text: `{"formatter":{},"lsp":{}}` },
    "formatter-lsp-mixed": {
      group: "current",
      text: JSON.stringify({
        formatter: { prettier: { command: [1] } },
        lsp: { typescript: { command: [1] } },
      }),
    },
    "instructions-invalid-root": { group: "current", text: `{"instructions": {}}` },
    "instructions-mixed": {
      group: "current",
      text: `{"instructions": ["CONTRIBUTING.md", 2, "https://example.com/rules.md"]}`,
    },
    "integer-like-record-keys": {
      group: "current",
      text: `{"agents": {"a": {"system": "a"}, "2": {"system": "two"}, "10": {"system": "ten"}}}`,
    },
    "invalid-file-malformed": { group: "current", text: `{ invalid` },
    "keeps-native-fields-with-legacy-snapshot": {
      group: "current",
      text: JSON.stringify({ snapshot: false, agents: { reviewer: { system: "Use V2" } } }),
    },
    "legacy-attachment": {
      group: "current",
      text: `{"attachment": {"image": {"auto_resize": false, "max_width": 1200}}}`,
    },
    "legacy-autoshare": { group: "current", text: `{"autoshare": true}` },
    "legacy-autoupdate-conflict": { group: "current", text: `{"autoupdate": "notify", "update": "auto"}` },
    "legacy-autoupdate-false": { group: "current", text: `{"autoupdate": false}` },
    "legacy-autoupdate-notify": { group: "current", text: `{"autoupdate": "notify"}` },
    "legacy-autoupdate-true": { group: "current", text: `{"autoupdate": true}` },
    "legacy-snapshot": { group: "current", text: `{"snapshot": false}` },
    "malformed-document": { group: "current", text: `{ invalid` },
    "mcp-enabled-only": { group: "current", text: `{"mcp":{"servers":{"enabled":true},"timeout":{"enabled":false}}}` },
    "mcp-experimental-timeout": {
      group: "current",
      text: `{"experimental": {"mcp_timeout": 5000}, "mcp": {"timeout": {"startup": 1000, "catalog": 6000}}}`,
    },
    "mcp-experimental-timeout-invalid": {
      group: "current",
      text: `{"experimental": {"mcp_timeout": 0}}`,
    },
    "mcp-invalid-root": { group: "current", text: `{"mcp": []}` },
    "mcp-native": {
      group: "current",
      text: `{"mcp": {"timeout": {"startup": 1000, "catalog": 2000}, "servers": {"local": {"type": "local", "command": ["node", "server.js"], "environment": {"KEY": "value"}, "codemode": false, "timeout": {"catalog": 3000}, "protocol": "legacy"}, "remote": {"type": "remote", "url": "https://example.com/mcp", "oauth": {"client_id": "client"}, "disabled": true}, "invalid": {"type": "local", "command": [1]}}}}`,
    },
    "mcp-numeric-timeouts": {
      group: "current",
      text: JSON.stringify({
        mcp: {
          timeout: 4000,
          servers: {
            local: { type: "local", command: ["local"], timeout: 6000 },
            remote: { type: "remote", url: "https://remote.example.com", timeout: 8000 },
            structured: { type: "local", command: ["structured"], timeout: { catalog: 9000 } },
            invalid: { type: "local", command: ["invalid"], timeout: -1 },
          },
        },
      }),
    },
    "mcp-timeout-empty": { group: "current", text: `{"mcp":{"timeout":{}}}` },
    "mcp-timeout-leaves": {
      group: "current",
      text: JSON.stringify({ mcp: { timeout: { execution: 3000, startup: "invalid", catalog: 2000 } } }),
    },
    "mcp-timeout-unknown": { group: "current", text: `{"mcp":{"timeout":{"unknown":1000}}}` },
    "media-conflict": {
      group: "current",
      text: `{"attachment": {"image": {"max_width": 1200}}, "media": {"image": {"max_width": 900}}}`,
    },
    "model-explicit-provider": {
      group: "current",
      text: `{"model":{"providerID":"azure-cognitive-services","model":"deployment"}}`,
    },
    "model-short": { group: "current", text: `{"model":"anthropic/claude"}` },
    "model-string-variant": { group: "current", text: `{"model":"google-vertex-anthropic/claude#high"}` },
    "native-atomic": {
      group: "current",
      text: `{"$schema": "schema.json", "shell": "/bin/zsh", "default_agent": "reviewer", "share": "disabled", "enterprise": {"url": "https://share.example.com"}, "username": "user", "timezone": "America/Los_Angeles", "automations": false, "snapshots": true, "media": {"image": {"max_width": 1200}}, "tool_output": {"max_lines": 1000, "max_bytes": 32768}, "websearch": {"provider": "exa"}, "warming": {"prompt": "keep warm", "interval": "4 minutes", "duration": "30 minutes"}}`,
    },
    "native-atomic-invalid": {
      group: "current",
      text: `{"share": "sometimes", "enterprise": "invalid", "tool_output": {"max_lines": 0}, "websearch": true, "warming": {"interval": "soon"}}`,
    },
    "number-1e21": { group: "current", text: `{"tool_output": {"max_lines": 1e21}}` },
    "number-negative-zero": { group: "current", text: `{"compaction": {"buffer": -0}}` },
    "omits-malformed-collection-roots": {
      group: "current",
      text: JSON.stringify({
        commands: [],
        providers: "invalid",
        references: false,
        agents: 1,
        permissions: {},
        instructions: {},
      }),
    },
    "plugins-legacy-and-native": {
      group: "current",
      text: `{"plugin": ["legacy-package", ["tuple-package", {"flag": true}], 1], "plugins": ["native-package", {"package": "object-package", "options": {"count": 2}}]}`,
    },
    "policies-enabled-all-invalid": { group: "current", text: JSON.stringify({ enabled_providers: [1, false] }) },
    "policies-enabled-empty": { group: "current", text: JSON.stringify({ enabled_providers: [] }) },
    "policies-enabled-mixed": {
      group: "current",
      text: JSON.stringify({ enabled_providers: [1, "anthropic", false] }),
    },
    "policies-enabled-not-array": { group: "current", text: JSON.stringify({ enabled_providers: "anthropic" }) },
    "policies-integration-prefixes": {
      group: "current",
      text: JSON.stringify({
        policies: [
          { action: "integration.use", resource: "mcp:github", effect: "deny" },
          { action: "integration.use", resource: "*", effect: "deny" },
        ],
      }),
    },
    "policies-sources": {
      group: "current",
      text: JSON.stringify({
        enabled_providers: ["anthropic"],
        disabled_providers: ["openai"],
        policies: [{ action: "provider.use", resource: "native", effect: "deny" }],
        experimental: {
          portable_shell_scanner: true,
          todo: true,
          media_blob: true,
          part_rows: true,
          image_generate: { model: "openai/gpt-image-2" },
          subagent_depth: 0,
          location_idle_timeout: "2 hours",
          policies: [{ action: "provider.use", resource: "custom", effect: "allow" }],
        },
      }),
    },
    "preserves-explicit-presence": {
      group: "current",
      text: JSON.stringify({
        snapshot: false,
        autoshare: false,
        references: {},
        commands: {},
        agents: {},
        providers: {},
        instructions: [],
        experimental: { subagent_depth: 0, location_idle_timeout: false },
      }),
    },
    "providers-partial-capabilities": {
      group: "current",
      text: JSON.stringify({
        providers: {
          demo: {
            name: "Demo",
            models: { demo: { name: "Demo", capabilities: { input: ["text"], output: ["text"] } } },
          },
        },
      }),
    },
    "recovers-list-items": {
      group: "current",
      text: JSON.stringify({
        skills: { paths: ["./skills", 1], urls: [false, "https://example.com/skills"] },
        instructions: ["one", 2, "three"],
        permissions: [
          { action: "read", resource: "*", effect: "allow" },
          { action: "read", resource: "*", effect: "invalid" },
        ],
      }),
    },
    "references-merge-conflict": {
      group: "current",
      text: `{"reference": {"duplicate": {"path": "../old"}, "legacy": "github.com/legacy"}, "references": {"duplicate": {"path": "../new"}, "native": {"repository": "github.com/native", "branch": "main"}}}`,
    },
    "root-array": { group: "current", text: `[]` },
    "root-boolean": { group: "current", text: `true` },
    "root-null": { group: "current", text: `null` },
    "root-number": { group: "current", text: `1` },
    "root-string": { group: "current", text: `"config"` },
    "share-conflict": { group: "current", text: `{"autoshare": true, "share": "manual"}` },
    "skills-invalid": { group: "current", text: `{"skills": "not-a-list-or-object"}` },
    "skills-list": { group: "current", text: `{"skills": ["./skills", 1, "~/shared"]}` },
    "skills-object": {
      group: "current",
      text: `{"skills": {"paths": ["./skills", false], "urls": ["https://example.com/skills", 2]}}`,
    },
    "snapshots-conflict": { group: "current", text: `{"snapshot": false, "snapshots": true}` },
    "supports-scalar-and-resource-config": {
      group: "current",
      text: JSON.stringify({
        shell: "/bin/bash",
        model: "anthropic/claude",
        default_agent: "reviewer",
        update: "notify",
        share: "disabled",
        enterprise: { url: "https://share.example.com" },
        username: "test-user",
        permissions: [
          { action: "bash", resource: "*", effect: "ask" },
          { action: "bash", resource: "git status", effect: "allow" },
        ],
        agents: {
          reviewer: {
            model: "openrouter/openai/gpt-5#high",
            request: {
              headers: { "x-agent": "reviewer" },
              body: { reasoningEffort: "high" },
            },
            description: "Review changes for correctness",
            system: "Find regressions.",
            mode: "subagent",
            hidden: false,
            color: "#ff6b6b",
            steps: 12,
            disabled: false,
            permissions: [{ action: "edit", resource: "*", effect: "deny" }],
          },
        },
        snapshots: false,
        watcher: { ignore: ["node_modules/**", "dist/**", ".git"] },
        formatter: {
          prettier: { disabled: true },
          custom: { command: ["custom-fmt", "$FILE"], extensions: [".foo"] },
        },
        lsp: { typescript: { disabled: true }, custom: { command: ["custom-lsp"], extensions: [".foo"] } },
        media: {
          image: { auto_resize: false, max_width: 1200, max_height: 900, max_base64_bytes: 1048576 },
        },
        tool_output: { max_lines: 1000, max_bytes: 32768 },
        mcp: {
          timeout: { startup: 5000, catalog: 60000, execution: 43200000 },
          servers: {
            local: {
              type: "local",
              command: ["node", "./mcp/server.js"],
              environment: { API_KEY: "secret" },
              disabled: false,
              codemode: false,
              timeout: { catalog: 10000 },
              protocol: "legacy",
            },
            remote: {
              type: "remote",
              url: "https://mcp.example.com/mcp",
              headers: { Authorization: "Bearer token" },
              oauth: { client_id: "client", scope: "read write", callback_port: 19876 },
              disabled: true,
              codemode: false,
              timeout: { startup: 15000 },
              protocol: "2026-07-28",
            },
          },
        },
        compaction: {
          auto: true,
          prune: false,
          keep: { tokens: 2000 },
          buffer: 10000,
        },
        skills: ["./skills", "~/shared-skills", "https://example.com/.well-known/skills/"],
        instructions: ["CONTRIBUTING.md", ".cursor/rules/*.md", "https://example.com/shared-rules.md"],
        references: {
          local: { path: "../library" },
          sdk: { repository: "github.com/example/sdk", branch: "main" },
          shorthand: "github.com/example/docs",
        },
      }),
    },
    "tool-use-policy-order": {
      group: "current",
      text: JSON.stringify({
        experimental: {
          policies: [
            { action: "tool.use", effect: "deny", resource: "shell:*" },
            { action: "tool.use", effect: "allow", resource: "shell:git *" },
            { action: "tool.use", effect: "deny", resource: "shell:git push *" },
          ],
        },
      }),
    },
    "unsupported-experimental": {
      group: "current",
      text: `{"experimental": {"disable_paste_summary": true, "batch_tool": true, "openTelemetry": true, "primary_tools": ["a"], "continue_loop_on_deny": true}}`,
    },
    "unsupported-policy-actions": {
      group: "current",
      text: JSON.stringify({
        experimental: {
          policies: [
            { action: "permission", effect: "allow", resource: "*" },
            { action: "tool.use", effect: "deny", resource: "shell:*" },
            { action: "future.use", effect: "deny", resource: "*" },
          ],
        },
      }),
    },
    "unsupported-top-level": {
      group: "current",
      text: `{"logLevel": "DEBUG", "server": {}, "subagent_depth": 2, "layout": {}}`,
    },
    "v1-agent-permission-order": {
      group: "v1",
      text: JSON.stringify({
        agent: {
          build: {
            tools: { write: false, read: true },
            permission: { "*": "allow", bash: "ask", custom: "deny", edit: "allow" },
          },
        },
      }),
    },
    "v1-agent-options-and-legacy-fields": {
      group: "v1",
      text: JSON.stringify({
        agent: {
          reviewer: {
            prompt: "Review changes",
            options: { reasoningEffort: "high", nested: { enabled: true } },
            temperature: 0.3,
            top_p: 0.9,
            color: "primary",
            steps: 10,
            maxSteps: 20,
            disable: true,
            hidden: true,
            customSetting: "extra",
            extraList: [1, 2],
          },
        },
      }),
    },
    "v1-agents-precedence": {
      group: "v1",
      text: JSON.stringify({
        agent: { reviewer: { prompt: "agent" }, agentOnly: { prompt: "agent-only" } },
        mode: { reviewer: { prompt: "mode" }, modeOnly: { prompt: "mode-only" } },
        agents: { reviewer: { system: "native" }, nativeOnly: { system: "native-only" } },
      }),
    },
    "v1-command-subtask-agent-description": {
      group: "v1",
      text: JSON.stringify({
        command: { deploy: { template: "deploy", description: "Deploy the app", agent: "build", subtask: true } },
      }),
    },
    "v1-canonical-equal-duplicates": {
      group: "v1",
      text: JSON.stringify({
        snapshot: false,
        snapshots: false,
        reference: { docs: { path: "../docs" } },
        references: { docs: { path: "../docs" } },
        agent: { reviewer: { prompt: "same" } },
        agents: { reviewer: { system: "same" } },
        provider: { custom: { name: "same" } },
        providers: { custom: { name: "same" } },
        compaction: { preserve_recent_tokens: 1000, keep: { tokens: 1000 } },
      }),
    },
    "v1-fixture-formatter-lsp-skills": {
      group: "v1",
      text: JSON.stringify({
        formatter: {
          prettier: {
            command: ["prettier", "--write", "$FILE"],
            extensions: [".ts"],
            environment: { NODE_ENV: "production" },
          },
          custom: { disabled: true },
        },
        lsp: {
          typescript: {
            command: ["tsc", "--lsp"],
            extensions: [".ts"],
            env: { NODE_ENV: "production" },
            initialization: { tsserver: { maxTsServerMemory: 3072 } },
          },
        },
        skills: { paths: ["./skills"], urls: ["https://example.com/.well-known/skills/"] },
      }),
    },
    "v1-fixture-mcp-options": {
      group: "v1",
      text: JSON.stringify({
        mcp: {
          local: {
            type: "local",
            command: ["node", "server.js"],
            cwd: "./server",
            environment: { KEY: "value" },
            enabled: false,
            codemode: false,
            timeout: 3000,
          },
          remote: {
            type: "remote",
            url: "https://example.com/mcp",
            headers: { Authorization: "Bearer token" },
            oauth: {
              clientId: "client",
              clientSecret: "secret",
              scope: "read write",
              callbackPort: 19876,
              redirectUri: "http://127.0.0.1:19876/mcp/oauth/callback",
            },
            enabled: true,
            timeout: 9000,
          },
        },
      }),
    },
    "v1-fixture-top-level": {
      group: "v1",
      text: JSON.stringify({
        layout: "auto",
        server: { port: 4096, hostname: "127.0.0.1", mdns: false, cors: ["https://app.example.com"] },
        logLevel: "DEBUG",
        attachment: {
          image: { auto_resize: false, max_width: 1200, max_height: 900, max_base64_bytes: 1048576 },
        },
      }),
    },
    "v1-fixture-tool-output-enterprise": {
      group: "v1",
      text: JSON.stringify({
        enterprise: { url: "https://enterprise.example.com" },
        tool_output: { max_lines: 1200, max_bytes: 40960 },
        experimental: { subagent_depth: 2, mcp_timeout: 7000 },
      }),
    },
    "v1-invalid-provider-overlays": {
      group: "v1",
      text: JSON.stringify({
        provider: {
          headers: { options: { headers: { valid: "yes", invalid: 1 } } },
          body: { options: { body: "not-an-object" } },
          valid: { options: { headers: { valid: "yes" }, body: { trace: true } } },
        },
      }),
    },
    "v1-invalid-small-model": {
      group: "v1",
      text: JSON.stringify({ small_model: "do-not-log-this-value" }),
    },
    "v1-mcp-codemode-false-local": {
      group: "v1",
      text: JSON.stringify({ mcp: { example: { type: "local", command: ["example"], codemode: false } } }),
    },
    "v1-mcp-codemode-false-remote": {
      group: "v1",
      text: JSON.stringify({
        mcp: { example: { type: "remote", url: "https://example.com/mcp", codemode: false } },
      }),
    },
    "v1-mcp-codemode-unset-local": {
      group: "v1",
      text: JSON.stringify({ mcp: { example: { type: "local", command: ["example"] } } }),
    },
    "v1-mcp-codemode-unset-remote": {
      group: "v1",
      text: JSON.stringify({ mcp: { example: { type: "remote", url: "https://example.com/mcp" } } }),
    },
    "v1-mcp-legacy-and-native": {
      group: "v1",
      text: JSON.stringify({
        experimental: { mcp_timeout: 5000 },
        mcp: {
          legacy: { type: "local", command: ["legacy"] },
          duplicate: { type: "remote", url: "https://legacy.example.com" },
          servers: {
            native: { type: "local", command: ["native"] },
            duplicate: { type: "remote", url: "https://native.example.com" },
            invalid: { type: "local", command: [1] },
          },
          timeout: { startup: 1000, catalog: 6000 },
        },
      }),
    },
    "v1-mcp-oauth-false": {
      group: "v1",
      text: JSON.stringify({
        mcp: { remote: { type: "remote", url: "https://example.com/mcp", oauth: false } },
      }),
    },
    "v1-mcp-reserved-names": {
      group: "v1",
      text: JSON.stringify({
        mcp: {
          servers: { type: "local", command: ["reserved-servers"] },
          timeout: { type: "remote", url: "https://reserved.example.com" },
        },
      }),
    },
    "v1-merges-named-maps": {
      group: "v1",
      text: JSON.stringify({
        reference: { legacy: { path: "../legacy" }, duplicate: { path: "../old" } },
        references: { native: { path: "../native" }, duplicate: { path: "../new" } },
        command: { legacy: { template: "legacy" }, duplicate: { template: "old" } },
        commands: { native: { template: "native" }, duplicate: { template: "new" } },
      }),
    },
    "v1-mode-permission-order": {
      group: "v1",
      text: JSON.stringify({
        mode: {
          build: {
            tools: { write: false, read: true },
            permission: { "*": "allow", bash: "ask", custom: "deny", edit: "allow" },
          },
        },
      }),
    },
    "v1-model-variants": {
      group: "v1",
      text: JSON.stringify({
        small_model: "anthropic/claude-haiku-4-5#low",
        agent: { reviewer: { model: "anthropic/claude#high", variant: "low" } },
        command: { review: { template: "review", model: "anthropic/claude#high" } },
      }),
    },
    "v1-permission-action-only-object": {
      group: "v1",
      text: JSON.stringify({
        permission: {
          question: { "*": "allow" },
          websearch: { "example.com": "deny" },
          doom_loop: { "*": "ask" },
        },
      }),
    },
    "v1-permission-redaction": {
      group: "v1",
      text: JSON.stringify({
        permission: { bash: { "curl -H Authorization:Bearer TOPSECRET *": "bogus" } },
      }),
    },
    "v1-permission-source-order": {
      group: "v1",
      text: JSON.stringify({
        tools: { bash: true, write: false },
        permission: { read: "allow", custom: { first: "deny", second: "ask" }, task: "allow" },
        permissions: [{ action: "native", resource: "*", effect: "deny" }],
      }),
    },
    "v1-provider-api-and-model-provider": {
      group: "v1",
      text: JSON.stringify({
        provider: {
          custom: {
            api: "https://api.example.com/v1",
            npm: "@ai-sdk/openai-compatible",
            models: {
              model: {
                provider: { npm: "@ai-sdk/openai", api: "https://model.example.com/v1" },
              },
            },
          },
        },
      }),
    },
    "v1-provider-compatible-body": {
      group: "v1",
      text: JSON.stringify({
        provider: {
          compatible: {
            npm: "@ai-sdk/openai-compatible",
            options: {
              reasoningEffort: "high",
              timeout: 1000,
              allowed_openai_params: ["reasoning_effort"],
              chat_template_kwargs: { enable_thinking: true },
            },
            models: {
              model: {
                options: { reasoningEffort: "low", taskBudget: 1024, metadata: { userId: "user" } },
                variants: { fast: { disabled: true, reasoningEffort: "low", extra: true } },
              },
            },
          },
        },
      }),
    },
    "v1-provider-empty-api-and-npm": {
      group: "v1",
      text: JSON.stringify({
        provider: {
          custom: {
            npm: "",
            api: "",
            models: { model: { provider: { npm: "", api: "" } } },
          },
        },
      }),
    },
    "v1-provider-invalid-options-tls": {
      group: "v1",
      text: JSON.stringify({ provider: { x: { options: { tls: 1 } } } }),
      throws: {
        diagnostic: { kind: "invalid", path: ["provider", "x"], message: "skipped malformed recognized value" },
      },
    },
    "v1-provider-model-costs": {
      group: "v1",
      text: JSON.stringify({
        provider: {
          custom: {
            models: {
              model: {
                id: "model-id",
                name: "Model",
                family: "family",
                cost: {
                  input: 1.5,
                  output: 2.5,
                  cache_read: 0.3,
                  cache_write: 0.4,
                  context_over_200k: { input: 3, output: 5, cache_read: 0.5, cache_write: 0.6 },
                },
                limit: { context: 200000, input: 180000, output: 8192 },
                modalities: { input: ["text", "image"], output: ["text"] },
                tool_call: true,
                interleaved: { field: "reasoning_content" },
                status: "deprecated",
                headers: { "x-model": "1" },
              },
            },
          },
        },
      }),
    },
    "v1-provider-options-split": {
      group: "v1",
      text: JSON.stringify({
        provider: {
          custom: {
            name: "Custom",
            npm: "@ai-sdk/openai-compatible",
            options: {
              apiKey: "secret",
              baseURL: "https://openai.example/v1",
              organization: "org",
              headers: { "x-test": "1", invalid: true },
              body: { store: true },
              nested: { camelCase: true },
            },
          },
        },
      }),
    },
    "v1-provider-options-valid-overlays": {
      group: "v1",
      text: JSON.stringify({
        provider: {
          custom: {
            name: "Custom",
            npm: "@ai-sdk/openai-compatible",
            options: {
              apiKey: "secret",
              baseURL: "https://openai.example/v1",
              organization: "org",
              headers: { "x-test": "1" },
              body: { store: true },
              nested: { camelCase: true },
            },
          },
        },
      }),
    },
    "v1-provider-model-thinking-key-order": {
      group: "v1",
      text: JSON.stringify({
        provider: {
          custom: {
            npm: "x",
            models: {
              removed: { options: { thinking: { blockBinding: false }, a: 1, b: 2 } },
              kept: { options: { thinking: { blockBinding: false, budget: 1, type: "x" }, a: 1, b: 2 } },
            },
          },
        },
      }),
    },
    "v1-provider-options-unknown-order": {
      group: "v1",
      text: JSON.stringify({
        provider: { custom: { options: { foo: 1, apiKey: "a", baseURL: "b", bar: 2 } } },
      }),
    },
    "v1-provider-retired-azure-override": {
      group: "v1",
      text: JSON.stringify({
        provider: {
          "azure-cognitive-services": {
            npm: "@ai-sdk/openai-compatible",
            env: ["AZURE_COGNITIVE_SERVICES_RESOURCE_NAME", "AZURE_API_KEY"],
            options: { baseURL: "https://custom.example/openai" },
          },
          "google-vertex-anthropic": { models: { claude: { name: "Claude" } } },
        },
      }),
    },
    "v1-provider-set-cache-key-order": {
      group: "v1",
      text: JSON.stringify({
        provider: { custom: { options: { setCacheKey: true, apiKey: "a", baseURL: "b", bar: 2 } } },
      }),
    },
    "v1-variant-disabled-order": {
      group: "v1",
      text: JSON.stringify({
        provider: {
          custom: {
            npm: "x",
            models: { model: { variants: { v: { zz: 1, disabled: false, apiKey: "k" } } } },
          },
        },
      }),
    },
    "v1-variant-disabled-order-compatible": {
      group: "v1",
      text: JSON.stringify({
        provider: {
          custom: {
            npm: "@ai-sdk/openai-compatible",
            models: { model: { variants: { v: { zz: 1, disabled: false, apiKey: "k" } } } },
          },
        },
      }),
    },
    "v1-recovers-malformed-entries": {
      group: "v1",
      text: JSON.stringify({
        command: { fallback: { template: "legacy" } },
        commands: {
          fallback: { template: 1 },
          valid: { template: "native" },
          invalid: { template: false },
        },
        providers: {
          valid: { name: "Valid" },
          invalid: { env: [1] },
        },
      }),
    },
    "v1-retired-provider-alias": {
      group: "v1",
      text: JSON.stringify({
        provider: {
          "azure-cognitive-services": { models: { deployment: {} } },
          azure: { env: [1] },
        },
      }),
    },
    "v1-set-cache-key": {
      group: "v1",
      text: JSON.stringify({
        provider: {
          proxy: {
            npm: "@ai-sdk/openai-compatible",
            options: { baseURL: "https://proxy.test/v1", setCacheKey: true },
          },
          openai: { options: { setCacheKey: false } },
          untouched: { options: { baseURL: "https://untouched.test/v1" } },
        },
      }),
    },
    "v1-small-model": {
      group: "v1",
      text: JSON.stringify({ small_model: "anthropic/claude-haiku-4-5" }),
    },
    "v1-small-model-title-agent": {
      group: "v1",
      text: JSON.stringify({
        small_model: "anthropic/claude-haiku-4-5",
        agent: { title: { prompt: "Custom title prompt" } },
      }),
    },
    "v1-thinking-block-binding": {
      group: "v1",
      text: JSON.stringify({
        provider: {
          gateway: {
            models: {
              anthropic: { options: { thinking: { type: "adaptive", blockBinding: false }, effort: "high" } },
              bedrock: { options: { reasoningConfig: { blockBinding: false } } },
              both: {
                options: {
                  thinking: { type: "adaptive", blockBinding: false },
                  reasoningConfig: { type: "adaptive", blockBinding: false },
                },
              },
              untouched: { options: { thinking: { type: "adaptive" } } },
            },
          },
        },
      }),
    },
    "v1-tool-permission-renames": {
      group: "v1",
      text: JSON.stringify({
        tools: { apply_patch: false, task: true, bash: false },
        permission: { apply_patch: "deny", task: "allow", bash: "ask" },
        agent: {
          reviewer: {
            tools: { apply_patch: false, task: true },
            permission: { apply_patch: "deny", task: "allow", bash: "ask" },
          },
        },
      }),
    },
    "v1-agent-permission-renamed-media-tools": {
      group: "v1",
      text: JSON.stringify({ agent: { reviewer: { permission: { experimental_image_generate: "allow" } } } }),
    },
    "v1-permission-renamed-media-tools": {
      group: "v1",
      text: JSON.stringify({
        permission: { experimental_image_generate: "allow", experimental_video_generate: { "*": "ask" } },
      }),
    },
    "v1-tools-renamed-media-tools": {
      group: "v1",
      text: JSON.stringify({ tools: { experimental_image_generate: true, experimental_video_generate: false } }),
    },
    "v1-unsupported-legacy-settings": {
      group: "v1",
      text: JSON.stringify({
        logLevel: "DEBUG",
        agent: { reviewer: { name: "do-not-log-this-value", prompt: "review" } },
        provider: {
          custom: {
            id: "do-not-log-this-value",
            whitelist: ["model"],
            models: {
              model: {
                release_date: "do-not-log-this-value",
                status: "active",
                interleaved: true,
              },
            },
          },
        },
        experimental: { openTelemetry: true },
      }),
    },
    "v1-unsupported-model-selections": {
      group: "v1",
      text: JSON.stringify({
        command: {
          invalidModel: { template: "one", model: "invalid" },
          invalidVariant: { template: "two", model: "anthropic/model", variant: "bad#variant" },
          missingModel: { template: "three", variant: "high" },
        },
        agent: { invalid: { prompt: "agent", model: "invalid", variant: "" } },
      }),
    },
    "valid-file-beside-invalid": { group: "current", text: `{"$schema":"base"}` },
    "warming-boolean": { group: "current", text: `{"warming": true}` },
    "warming-canonicalization": {
      group: "current",
      text: JSON.stringify({ warming: { interval: "4 minutes", duration: "30 minutes" } }),
    },
    "watcher-invalid": { group: "current", text: `{"watcher": {"ignore": "not-a-list"}}` },
    "watcher-mixed": { group: "current", text: `{"watcher": {"ignore": ["node_modules/**", 1]}}` },
    "worktree-invalid": { group: "current", text: `{"worktree": {"directory": "   "}}` },
    "worktree-trim": { group: "current", text: `{"worktree": {"directory": "  ../worktrees  "}}` },
  }

  const settle = (value: unknown) => JSON.parse(JSON.stringify(value))
  const lines = Object.keys(cases)
    .sort()
    .map((name) => {
      const item = cases[name]!
      return `${JSON.stringify(name)}:${JSON.stringify(settle(run(item)))}`
    })
  process.stdout.write(`{\n${lines.join(",\n")}\n}\n`)

  function run(item: NormalizeCase) {
    const errors: ParseError[] = []
    const input: unknown = parse(item.text, errors, { allowTrailingComma: true })
    if (errors.length > 0)
      return {
        group: item.group,
        input: item.text,
        normalized: null,
        parsed: {
          type: "rejected",
          stage: "malformed",
          diagnostics: [
            {
              kind: "invalid",
              path: ["$"],
              message: "rejected malformed JSON or JSONC document",
            },
          ],
        },
      }
    try {
      const result = ConfigNormalize.normalize(input)
      if (result.type === "rejected")
        return {
          group: item.group,
          input: item.text,
          normalized: result,
          parsed: { type: "rejected", stage: "normalize", diagnostics: result.diagnostics },
        }
      const info = Option.getOrUndefined(decodeInfo(result.encoded))
      if (info !== undefined)
        return {
          group: item.group,
          input: item.text,
          normalized: result,
          parsed: { type: "info", info: Schema.encodeSync(Info)(info) },
        }
      return {
        group: item.group,
        input: item.text,
        normalized: result,
        parsed: {
          type: "rejected",
          stage: "final",
          diagnostics: [
            ...result.diagnostics,
            {
              kind: "invalid",
              path: ["$"],
              message: "rejected canonical configuration after final validation",
            },
          ],
        },
      }
    } catch (error) {
      if (!item.throws) throw error
      return {
        group: item.group,
        input: item.text,
        throws: true,
        diagnostic: item.throws.diagnostic,
      }
    }
  }
}

type SubstituteSource =
  | {
      type: "path"
      path: string
    }
  | {
      type: "virtual"
      source: string
      dir: string
    }

type SubstituteCase = {
  source: SubstituteSource
  text: string
  env: Record<string, string>
  files: Record<string, string>
}

async function exportSubstitute() {
  const root = process.env.OPENCODE_CORPUS_ROOT
  const home = process.env.HOME
  if (!root || !home) {
    console.error("corpus: substitute needs OPENCODE_CORPUS_ROOT and HOME")
    process.exit(1)
  }

  const config: SubstituteSource = { type: "path", path: "config/opencode.jsonc" }
  const cases: Record<string, SubstituteCase> = {
    "env-empty": {
      source: config,
      text: `{"shell":"{env:EMPTY_SHELL}"}`,
      env: { EMPTY_SHELL: "" },
      files: {},
    },
    "env-set": {
      source: config,
      text: `{"shell":"{env:SHELL}"}`,
      env: { SHELL: "/bin/zsh" },
      files: {},
    },
    "env-unset": {
      source: config,
      text: `{"shell":"{env:OPENCODE_RS_SUBSTITUTE_UNSET}"}`,
      env: {},
      files: {},
    },
    "env-windows-path": {
      source: config,
      text: `{"shell":"{env:WIN_PATH}"}`,
      env: { WIN_PATH: String.raw`C:\Users\me\config` },
      files: {},
    },
    "file-absolute": {
      source: config,
      text: `{"token":"{file:{{root}}/shared/absolute.txt}"}`,
      env: {},
      files: { "shared/absolute.txt": "absolute\n" },
    },
    "file-comment": {
      source: config,
      text: `   // {file:secrets/token.txt}
{"token":"ok"}`,
      env: {},
      files: { "config/secrets/token.txt": "commented\n" },
    },
    "file-escape": {
      source: config,
      text: `{"token":"{file:escape.txt}"}`,
      env: {},
      files: { "config/escape.txt": `  quoted "value" and C:\\path\nsecond line\n` },
    },
    "file-home": {
      source: config,
      text: `{"token":"{file:~/home-secret.txt}"}`,
      env: {},
      files: { "home/home-secret.txt": "home-secret\n" },
    },
    "file-missing": {
      source: config,
      text: `{"token":"{file:missing.txt}"}`,
      env: {},
      files: {},
    },
    "file-relative": {
      source: config,
      text: `{"token":"{file:secrets/token.txt}"}`,
      env: {},
      files: { "config/secrets/token.txt": "  secret-value\n" },
    },
    "file-trim": {
      source: config,
      text: `{"token":"{file:trim.txt}"}`,
      env: {},
      files: { "config/trim.txt": `\n\n  trimmed value  \n` },
    },
    "virtual-dir": {
      source: { type: "virtual", source: "virtual.jsonc", dir: "config" },
      text: `{"token":"{file:virtual.txt}"}`,
      env: {},
      files: { "config/virtual.txt": "virtual\n" },
    },
  }

  const layer = LayerNode.compile(FSUtil.node)
  delete process.env.OPENCODE_RS_SUBSTITUTE_UNSET
  const lines: Array<string> = []
  for (const name of Object.keys(cases).sort()) {
    const item = cases[name]!
    await rm(root, { recursive: true, force: true })
    await mkdir(root, { recursive: true })
    for (const [file, content] of Object.entries(item.files)) {
      const target = path.join(root, file)
      await mkdir(path.dirname(target), { recursive: true })
      await writeFile(target, content)
    }

    const source: SubstituteSource =
      item.source.type === "path"
        ? { type: "path", path: path.join(root, item.source.path) }
        : { type: "virtual", source: item.source.source, dir: path.join(root, item.source.dir) }
    const references = new Set<string>()
    const output = await Effect.runPromise(
      ConfigVariable.substitute({
        ...source,
        text: item.text.replaceAll("{{root}}", root),
        env: item.env,
        references,
      }).pipe(Effect.provide(layer), Effect.provide(Logger.layer([]))),
    )
    lines.push(
      `${JSON.stringify(name)}:${JSON.stringify({
        source: item.source,
        text: item.text,
        env: item.env,
        files: item.files,
        output,
        references: [...references].map((ref) => path.relative(root, ref).split(path.sep).join("/")).sort(),
      })}`,
    )
  }
  process.stdout.write(`{\n${lines.join(",\n")}\n}\n`)
}

type DiscoverCase = {
  directories?: string[]
  files?: Record<string, string>
  symlinks?: Record<string, string>
  modes?: Record<string, number>
  options?: {
    project?: boolean
    global?: boolean
    claude?: boolean
    system?: boolean | string
    file?: string
    content?: string
  }
  location: { directory: string; project_id: string }
  global: { config: string; home: string }
}

async function exportDiscover() {
  const corpus = process.env.OPENCODE_CORPUS_ROOT
  if (!corpus) {
    console.error("corpus: discover needs OPENCODE_CORPUS_ROOT")
    process.exit(1)
  }

  const g = { config: "global", home: "global/home" }
  const cases: Record<string, DiscoverCase> = {
    "absent-ancestor-candidates": {
      directories: ["ws/org/repo/.opencode"],
      files: { "ws/org/repo/packages/feature/opencode.json": "{}" },
      options: { system: false },
      location: { directory: "ws/org/repo/packages/feature", project_id: "global" },
      global: g,
    },
    "denied-ancestor-listing": {
      directories: ["guarded/.opencode", "guarded/project"],
      modes: { guarded: 0o111 },
      options: { system: false },
      location: { directory: "guarded/project", project_id: "global" },
      global: g,
    },
    "mixed-case-names": {
      directories: ["Workspace/Repo/.opencode", "Workspace/Repo/.claude", "Workspace/Repo/.agents"],
      files: { "Workspace/Repo/opencode.jsonc": "{}" },
      options: { system: false },
      location: { directory: "Workspace/Repo", project_id: "global" },
      global: g,
    },
    "ancestors-across-project-boundary": {
      directories: ["a/.opencode", "a/b/.opencode", "a/b/c/.opencode"],
      files: { "a/b/c/d/opencode.json": "{}" },
      options: { system: false },
      location: { directory: "a/b/c/d", project_id: "global" },
      global: g,
    },
    "global-disabled-home-claude-agents": {
      directories: ["global/home/.claude", "global/home/.agents", "global/home/project"],
      options: { global: false, system: false },
      location: { directory: "global/home/project", project_id: "global" },
      global: g,
    },
    "claude-disabled": {
      directories: ["global/home/.claude", "global/home/.agents", "project/.claude", "project/.agents"],
      options: { claude: false, system: false },
      location: { directory: "project", project_id: "global" },
      global: g,
    },
    "global-disabled-walk-reaches-global-file": {
      directories: ["global/plugins/demo"],
      files: { "global/opencode.json": "{}" },
      options: { global: false, system: false },
      location: { directory: "global/plugins/demo", project_id: "global" },
      global: g,
    },
    "location-inside-global-config": {
      directories: ["global/plugins/demo"],
      files: { "global/opencode.json": "{}" },
      options: { system: false },
      location: { directory: "global/plugins/demo", project_id: "global" },
      global: g,
    },
    "symlinked-global-root": {
      directories: ["real/.opencode"],
      files: { "real/.opencode/opencode.json": "{}" },
      symlinks: { link: "real" },
      options: { system: false },
      location: { directory: "link", project_id: "global" },
      global: { config: "real/.opencode", home: "global/home" },
    },
    "global-root-same-spelling": {
      directories: ["real/.opencode"],
      files: { "real/.opencode/opencode.json": "{}" },
      options: { system: false },
      location: { directory: "real", project_id: "global" },
      global: { config: "real/.opencode", home: "global/home" },
    },
    "system-directory": {
      directories: ["system", "project"],
      files: { "system/opencode.json": "{}", "global/opencode.json": "{}", "project/opencode.json": "{}" },
      options: { system: "system" },
      location: { directory: "project", project_id: "global" },
      global: g,
    },
    "project-disabled": {
      directories: ["project/.opencode"],
      files: { "project/opencode.json": "{}" },
      options: { project: false, system: false },
      location: { directory: "project", project_id: "global" },
      global: g,
    },
    "missing-project-directory": {
      options: { system: false },
      location: { directory: "project", project_id: "global" },
      global: g,
    },
    "explicit-file": {
      files: { "custom.json": "{}", "project/opencode.json": "{}" },
      options: { file: "custom.json", content: `{"shell":"content"}`, system: false },
      location: { directory: "project", project_id: "global" },
      global: g,
    },
    "global-enabled-home-directories": {
      directories: ["global/home/.claude", "global/home/.agents", "global/home/project"],
      options: { system: false },
      location: { directory: "global/home/project", project_id: "global" },
      global: g,
    },
    "overlay-present": {
      directories: ["global/project/proj_one"],
      options: { system: false },
      location: { directory: "project", project_id: "proj_one" },
      global: g,
    },
    "overlay-missing": {
      options: { system: false },
      location: { directory: "project", project_id: "proj_missing" },
      global: g,
    },
    "overlay-global-project": {
      directories: ["global/project/global"],
      options: { system: false },
      location: { directory: "project", project_id: "global" },
      global: g,
    },
    "overlay-global-disabled": {
      directories: ["global/project/proj_one"],
      options: { global: false, system: false },
      location: { directory: "project", project_id: "proj_one" },
      global: g,
    },
    "overlay-project-disabled": {
      directories: ["global/project/proj_one"],
      options: { project: false, system: false },
      location: { directory: "project", project_id: "proj_one" },
      global: g,
    },
    "overlay-home": {
      directories: ["global/project/home-project"],
      options: { system: false },
      location: { directory: "global/home", project_id: "home-project" },
      global: g,
    },
  }

  const fsLayer = LayerNode.compile(FSUtil.node)
  const lines: Array<string> = []
  for (const name of Object.keys(cases).sort()) {
    const item = cases[name]!
    const dir = path.join(corpus, name)
    await rm(dir, { recursive: true, force: true })
    await mkdir(dir, { recursive: true })
    const root = await realpath(dir)
    const inside = (value: string) => {
      const relative = path.relative(root, value)
      return relative === "" || (relative !== ".." && !relative.startsWith(`..${path.sep}`) && !path.isAbsolute(relative))
    }
    const rel = (value: string) => path.relative(root, value).split(path.sep).join("/")

    for (const directory of item.directories ?? []) await mkdir(path.join(root, directory), { recursive: true })
    for (const [file, content] of Object.entries(item.files ?? {})) {
      const target = path.join(root, file)
      await mkdir(path.dirname(target), { recursive: true })
      await writeFile(target, content)
    }
    for (const [link, target] of Object.entries(item.symlinks ?? {})) {
      const linkPath = path.join(root, link)
      await mkdir(path.dirname(linkPath), { recursive: true })
      await symlink(path.join(root, target), linkPath)
    }
    for (const [file, mode] of Object.entries(item.modes ?? {})) await chmod(path.join(root, file), mode)

    const globalLayer = Layer.succeed(
      Global.Service,
      Global.Service.of({
        home: path.join(root, item.global.home),
        config: path.join(root, item.global.config),
        data: path.join(root, ".runtime/data"),
        cache: path.join(root, ".runtime/cache"),
        state: path.join(root, ".runtime/state"),
        tmp: path.join(root, ".runtime/tmp"),
        bin: path.join(root, ".runtime/bin"),
        log: path.join(root, ".runtime/log"),
        repos: path.join(root, ".runtime/repos"),
      }),
    )
    const locationDirectory = AbsolutePath.make(path.join(root, item.location.directory))
    const locationLayer = Layer.succeed(
      Location.Service,
      Location.Service.of({
        directory: locationDirectory,
        workspaceID: undefined,
        project: {
          id: Project.ID.make(item.location.project_id),
          directory: locationDirectory,
          canonical: locationDirectory,
        },
      }),
    )
    const layer = Layer.mergeAll(fsLayer, globalLayer, locationLayer)
    const options = {
      project: item.options?.project,
      global: item.options?.global,
      claude: item.options?.claude,
      system:
        item.options?.system === false
          ? false
          : item.options?.system === undefined
            ? undefined
            : path.join(root, item.options.system),
      file: item.options?.file === undefined ? undefined : path.join(root, item.options.file),
      content: item.options?.content,
    }

    try {
      const sources = await Effect.runPromise(
        ConfigDiscovery.discover(options).pipe(Effect.provide(layer), Effect.provide(Logger.layer([]))),
      )
      const overlay = await Effect.runPromise(
        ConfigDiscovery.overlay(options).pipe(Effect.provide(layer), Effect.provide(Logger.layer([]))),
      )
      const plan = [...ConfigWatch.plan(sources).values()]
        .filter((target) => inside(target.path))
        .map((target) =>
          target.type === "directory"
            ? { path: rel(target.path), type: "directory", ignore: [...target.ignore] }
            : { path: rel(target.path), type: "entries", names: [...target.names].sort() },
        )
        .sort((a, b) => {
          const left = JSON.stringify(a)
          const right = JSON.stringify(b)
          return left < right ? -1 : left > right ? 1 : 0
        })
      lines.push(
        `${JSON.stringify(name)}:${JSON.stringify({
          files: item.files ?? {},
          directories: item.directories ?? [],
          symlinks: item.symlinks ?? {},
          modes: item.modes ?? {},
          options: item.options ?? {},
          location: item.location,
          global: item.global,
          sources: {
            system:
              sources.system && inside(sources.system.path)
                ? { path: rel(sources.system.path), present: sources.system.present }
                : null,
            preferences: sources.preferences && inside(sources.preferences) ? rel(sources.preferences) : null,
            global: sources.global && inside(sources.global) ? rel(sources.global) : null,
            explicit: sources.explicit && inside(sources.explicit) ? rel(sources.explicit) : null,
            direct: sources.direct.filter(inside).map(rel),
            project: sources.project
              .filter((entry) => inside(entry.path))
              .map((entry) => ({ path: rel(entry.path), present: entry.present })),
            claude: sources.claude.filter(inside).map(rel),
            agents: sources.agents.filter(inside).map(rel),
          },
          overlay: overlay && inside(overlay.path) ? { path: rel(overlay.path), present: overlay.present } : null,
          plan,
        })}`,
      )
    } finally {
      for (const file of Object.keys(item.modes ?? {})) await chmod(path.join(root, file), 0o755)
    }
  }
  process.stdout.write(`{\n${lines.join(",\n")}\n}\n`)
}

type LoadCredential = { key?: string; metadata?: Record<string, unknown> }
type LoadIntegration = {
  integrationID: string
  connection: unknown
  organization?: string
  info: ConfigIntegration.Remote
}
type LoadWellKnown = {
  entries?: Array<{ origin: string; integrationID: string; manifest: WellKnown.Manifest }>
  credentials?: Array<LoadCredential & { integrationID: string }>
  get?: Record<string, LoadCredential>
  resolve?: unknown[] | null
  resolveError?: boolean
  cached?: unknown[]
}
type LoadOptions = {
  project?: boolean
  global?: boolean
  claude?: boolean
  system?: boolean | string
  file?: string
  content?: string
}
type LoadCase = {
  directories?: string[]
  files?: Record<string, string>
  options?: LoadOptions
  location: { directory: string; project_id: string }
  global: { config: string; home: string }
  flags?: { channel?: string; overrides?: string | null }
  integrations?: LoadIntegration[]
  wellknown?: LoadWellKnown
  policy?: Array<{ action: ConfigPolicy.Action; resource: string }>
}

async function exportLoad() {
  const corpus = process.env.OPENCODE_CORPUS_ROOT
  if (!corpus) {
    console.error("corpus: load needs OPENCODE_CORPUS_ROOT")
    process.exit(1)
  }

  const g = { config: "global", home: "global/home" }
  const shell = (value: string) => JSON.stringify({ shell: value })
  const policy = (action: ConfigPolicy.Action, resource: string, effect: "allow" | "deny") => ({
    action,
    resource,
    effect,
  })
  const toolPolicy = (resource: string, effect: "allow" | "deny") => policy("tool.use", resource, effect)
  const integrationPolicy = (resource: string, effect: "allow" | "deny") =>
    policy("integration.use", resource, effect)

  const cases: Record<string, LoadCase> = {
    "empty-directories": {
      directories: ["project"],
      location: { directory: "project", project_id: "global" },
      global: g,
    },
    "json-and-jsonc-order": {
      directories: ["project"],
      files: {
        "project/opencode.json": JSON.stringify({ $schema: "json" }),
        "project/opencode.jsonc": JSON.stringify({ $schema: "jsonc" }),
      },
      location: { directory: "project", project_id: "global" },
      global: g,
    },
    "system-highest": {
      directories: ["system", "global", "project"],
      files: {
        "system/opencode.json": JSON.stringify({
          shell: "system",
          username: "system-user",
          policies: [toolPolicy("shell:*", "deny")],
        }),
        "global/opencode.json": shell("global"),
        "project/opencode.json": shell("project"),
      },
      options: { system: "system" },
      location: { directory: "project", project_id: "global" },
      global: g,
      policy: [{ action: "tool.use", resource: "shell:ls" }],
    },
    "system-disabled": {
      directories: ["system", "global", "project"],
      files: {
        "system/opencode.json": JSON.stringify({
          shell: "system",
          username: "system-user",
          policies: [toolPolicy("shell:*", "deny")],
        }),
        "global/opencode.json": shell("global"),
        "project/opencode.json": shell("project"),
      },
      options: { system: false },
      location: { directory: "project", project_id: "global" },
      global: g,
    },
    "explicit-content-priority": {
      directories: ["global", "project"],
      files: {
        "global/opencode.json": shell("global"),
        "custom.json": shell("explicit"),
        "project/opencode.json": shell("project"),
      },
      options: { file: "custom.json", content: shell("content") },
      location: { directory: "project", project_id: "global" },
      global: g,
    },
    "global-disabled": {
      directories: ["global", "global/home/.claude", "global/home/.agents", "project"],
      files: {
        "global/opencode.json": shell("global"),
        "project/opencode.json": shell("project"),
      },
      options: { global: false },
      location: { directory: "project", project_id: "global" },
      global: g,
    },
    "claude-disabled": {
      directories: [
        "global",
        "global/home/.claude",
        "global/home/.agents",
        "project",
        "project/.claude",
        "project/.agents",
      ],
      files: {
        "global/opencode.json": shell("global"),
        "project/opencode.json": shell("project"),
      },
      options: { claude: false },
      location: { directory: "project", project_id: "global" },
      global: g,
    },
    "project-disabled": {
      directories: ["global", "project"],
      files: {
        "global/opencode.json": shell("global"),
        "project/opencode.json": shell("project"),
      },
      options: { project: false },
      location: { directory: "project", project_id: "global" },
      global: g,
    },
    "ancestor-boundary": {
      directories: [
        "global",
        "global/home/.claude",
        "global/home/.agents",
        "repo/packages/app",
        "repo/.agents",
        "repo/.claude",
        "repo/.opencode",
        "repo/packages/app/.agents",
        "repo/packages/app/.claude",
        "repo/packages/app/.opencode",
      ],
      files: {
        "opencode.json": JSON.stringify({ $schema: "outside" }),
        "global/opencode.json": JSON.stringify({ $schema: "global" }),
        "repo/opencode.json": JSON.stringify({ $schema: "root" }),
        "repo/packages/opencode.jsonc": JSON.stringify({ $schema: "parent" }),
        "repo/packages/app/opencode.json": JSON.stringify({ $schema: "directory" }),
        "repo/.opencode/opencode.json": JSON.stringify({ $schema: "root-dot" }),
        "repo/packages/app/.opencode/opencode.jsonc": JSON.stringify({ $schema: "directory-dot" }),
      },
      location: { directory: "repo/packages/app", project_id: "global" },
      global: g,
    },
    "invalid-file-skipped": {
      directories: ["project"],
      files: {
        "project/opencode.json": JSON.stringify({ $schema: "valid" }),
        "project/opencode.jsonc": "{ invalid",
      },
      location: { directory: "project", project_id: "global" },
      global: g,
    },
    "missing-explicit-file": {
      directories: ["project"],
      files: { "project/opencode.json": shell("project") },
      options: { file: "missing.json" },
      location: { directory: "project", project_id: "global" },
      global: g,
    },
    "file-substitution": {
      directories: ["project"],
      files: {
        "project/token.txt": `  quoted "token"  \n`,
        "project/opencode.jsonc": `{
  // Ignored reference: {file:.}
  "username": "{file:token.txt}",
  "mcp": {
    "servers": {
      "remote": {
        "type": "remote",
        "url": "https://example.com/mcp",
        "headers": {
          "X-Token": "{file:token.txt}",
          "X-Folder": "{file:.}"
        }
      }
    }
  }
}`,
      },
      location: { directory: "project", project_id: "global" },
      global: g,
    },
    "managed-policies-first": {
      directories: ["system", "global", "project"],
      files: {
        "system/opencode.json": JSON.stringify({
          shell: "system",
          username: "system-user",
          policies: [toolPolicy("shell:*", "deny")],
        }),
        "global/opencode.json": shell("global"),
        "project/opencode.json": shell("project"),
      },
      options: { system: "system", content: shell("content") },
      location: { directory: "project", project_id: "global" },
      global: g,
      policy: [
        { action: "tool.use", resource: "shell:ls" },
        { action: "tool.use", resource: "shell:rm -rf" },
      ],
    },
    "managed-settings-last": {
      directories: ["system", "global", "project"],
      files: {
        "system/opencode.json": JSON.stringify({ shell: "system", policies: [toolPolicy("shell:*", "deny")] }),
        "global/opencode.json": shell("global"),
      },
      options: { system: "system" },
      location: { directory: "project", project_id: "global" },
      global: g,
      policy: [{ action: "tool.use", resource: "shell:ls" }],
    },
    "integration-order": {
      directories: ["global", "project"],
      files: {
        "global/opencode.json": JSON.stringify({ providers: { remote: { name: "global" } } }),
        "project/opencode.json": JSON.stringify({ providers: { remote: { name: "project" } } }),
      },
      location: { directory: "project", project_id: "global" },
      global: g,
      integrations: [
        {
          integrationID: "opencode",
          connection: { type: "env", name: "OPENCODE_API_KEY" },
          info: { providers: { remote: { name: "integration" } } },
        },
      ],
    },
    "integration-policy-rank": {
      directories: ["system", "global", "project"],
      files: {
        "system/opencode.json": JSON.stringify({ policies: [toolPolicy("shell:rm*", "deny")] }),
        "global/opencode.json": JSON.stringify({ policies: [toolPolicy("shell:*", "allow")] }),
        "project/opencode.json": JSON.stringify({ policies: [toolPolicy("shell:*", "allow")] }),
      },
      options: { system: "system" },
      location: { directory: "project", project_id: "global" },
      global: g,
      integrations: [
        {
          integrationID: "opencode",
          connection: { type: "env", name: "OPENCODE_API_KEY" },
          organization: "Acme",
          info: {
            policies: [toolPolicy("shell:rm*", "allow"), toolPolicy("shell:*", "deny")],
          },
        },
      ],
      policy: [
        { action: "tool.use", resource: "shell:rm -rf" },
        { action: "tool.use", resource: "shell:ls" },
      ],
    },
    "integration-denied-keeps-policies": {
      directories: ["system", "global", "project"],
      files: {
        "system/opencode.json": JSON.stringify({ policies: [integrationPolicy("*", "deny")] }),
        "global/opencode.json": shell("global"),
        "project/opencode.json": shell("project"),
      },
      options: { system: "system" },
      location: { directory: "project", project_id: "global" },
      global: g,
      integrations: [
        {
          integrationID: "opencode",
          connection: { type: "env", name: "OPENCODE_API_KEY" },
          info: {
            providers: { remote: { name: "integration" } },
            policies: [toolPolicy("shell:*", "deny")],
          },
        },
      ],
    },
    "integration-account-allow": {
      directories: ["system", "global", "project"],
      files: {
        "system/opencode.json": JSON.stringify({
          policies: [integrationPolicy("*", "deny"), integrationPolicy("opencode#org_allowed", "allow")],
        }),
        "global/opencode.json": shell("global"),
        "project/opencode.json": shell("project"),
      },
      options: { system: "system" },
      location: { directory: "project", project_id: "global" },
      global: g,
      integrations: [
        {
          integrationID: "opencode",
          connection: { type: "credential", id: "cred_1", label: "Console", method: "key" },
          info: { providers: { remote: { name: "integration" } } },
        },
      ],
      wellknown: { get: { cred_1: { metadata: { orgID: "org_allowed" } } } },
    },
    "wellknown-loads": {
      directories: ["global", "project"],
      files: {
        "global/opencode.json": shell("global"),
        "project/opencode.json": shell("project"),
      },
      location: { directory: "project", project_id: "global" },
      global: g,
      wellknown: {
        entries: [
          {
            origin: "https://example.com",
            integrationID: "https://example.com",
            manifest: { auth: { command: ["login"], env: "TOKEN" } },
          },
        ],
        credentials: [{ integrationID: "https://example.com", key: "secret" }],
        resolve: [{ shell: "{env:TOKEN}" }],
      },
    },
    "wellknown-missing-credential": {
      directories: ["global", "project"],
      files: {
        "global/opencode.json": shell("global"),
        "project/opencode.json": shell("project"),
      },
      location: { directory: "project", project_id: "global" },
      global: g,
      wellknown: {
        entries: [
          {
            origin: "https://example.com",
            integrationID: "https://example.com",
            manifest: { auth: { command: ["login"], env: "TOKEN" } },
          },
        ],
        resolve: [{ shell: "{env:TOKEN}" }],
      },
    },
    "wellknown-failure-cached": {
      directories: ["global", "project"],
      files: {
        "global/opencode.json": shell("global"),
        "project/opencode.json": shell("project"),
      },
      location: { directory: "project", project_id: "global" },
      global: g,
      wellknown: {
        entries: [
          {
            origin: "https://example.com",
            integrationID: "https://example.com",
            manifest: { auth: { command: ["login"], env: "TOKEN" } },
          },
        ],
        credentials: [{ integrationID: "https://example.com", key: "secret" }],
        resolveError: true,
        cached: [{ shell: "cached" }],
      },
    },
    "wellknown-failure-empty": {
      directories: ["global", "project"],
      files: {
        "global/opencode.json": shell("global"),
        "project/opencode.json": shell("project"),
      },
      location: { directory: "project", project_id: "global" },
      global: g,
      wellknown: {
        entries: [
          {
            origin: "https://example.com",
            integrationID: "https://example.com",
            manifest: { auth: { command: ["login"], env: "TOKEN" } },
          },
        ],
        credentials: [{ integrationID: "https://example.com", key: "secret" }],
        resolveError: true,
        cached: [],
      },
    },
    "wellknown-denied": {
      directories: ["system", "global", "project"],
      files: {
        "system/opencode.json": JSON.stringify({ policies: [integrationPolicy("*", "deny")] }),
        "global/opencode.json": shell("global"),
        "project/opencode.json": shell("project"),
      },
      options: { system: "system" },
      location: { directory: "project", project_id: "global" },
      global: g,
      wellknown: {
        entries: [
          {
            origin: "https://example.com",
            integrationID: "https://example.com",
            manifest: { auth: { command: ["login"], env: "TOKEN" } },
          },
        ],
        credentials: [{ integrationID: "https://example.com", key: "secret" }],
        resolve: [
          {
            providers: { remote: { name: "remote" } },
            policies: [toolPolicy("shell:*", "deny")],
          },
        ],
      },
    },
    "wellknown-invalid-config": {
      directories: ["global", "project"],
      files: {
        "global/opencode.json": shell("global"),
        "project/opencode.json": shell("project"),
      },
      location: { directory: "project", project_id: "global" },
      global: g,
      wellknown: {
        entries: [
          {
            origin: "https://example.com",
            integrationID: "https://example.com",
            manifest: { auth: { command: ["login"], env: "TOKEN" } },
          },
        ],
        credentials: [{ integrationID: "https://example.com", key: "secret" }],
        resolve: [null],
      },
    },
    "overlay-on": {
      directories: ["global", "project", "global/project/p1"],
      files: {
        "global/opencode.json": shell("global"),
        "project/opencode.json": shell("project"),
        "global/project/p1/opencode.json": shell("overlay"),
      },
      location: { directory: "project", project_id: "p1" },
      global: g,
      flags: { overrides: "project_overlay" },
    },
    "overlay-off": {
      directories: ["global", "project", "global/project/p1"],
      files: {
        "global/opencode.json": shell("global"),
        "project/opencode.json": shell("project"),
        "global/project/p1/opencode.json": shell("overlay"),
      },
      location: { directory: "project", project_id: "p1" },
      global: g,
    },
    "ignores-legacy-config": {
      directories: ["project"],
      files: {
        "project/config.json": shell("legacy"),
        "project/opencode.json": shell("project"),
      },
      location: { directory: "project", project_id: "global" },
      global: g,
    },
    "rejected-each-stage": {
      directories: ["system", "global", "project", "project/.opencode"],
      files: {
        "system/opencode.json": "{ invalid",
        "global/opencode.json": JSON.stringify({ $schema: "global" }),
        "custom.json": "[]",
        "project/opencode.json": "{ invalid",
        "project/.opencode/opencode.json": "[]",
      },
      options: { system: "system", file: "custom.json", content: "[]" },
      location: { directory: "project", project_id: "global" },
      global: g,
    },
  }

  const encodeEntry = Schema.encodeSync(Entry)
  const log = console.log
  const lines: string[] = []

  for (const name of Object.keys(cases).sort()) {
    const item = cases[name]!
    const candidate = path.join(corpus, "load", name)
    await rm(candidate, { recursive: true, force: true })
    await mkdir(candidate, { recursive: true })
    const root = await realpath(candidate)
    const inside = (value: string) => insideRoot(root, value)
    const rel = (value: string) => relativeToRoot("load", name, root, value)
    const previousFlags = process.env.OPENCODE_FLAGS
    try {
      for (const directory of item.directories ?? []) await mkdir(path.join(root, directory), { recursive: true })
      for (const [file, content] of Object.entries(item.files ?? {})) {
        const target = path.join(root, file)
        await mkdir(path.dirname(target), { recursive: true })
        await writeFile(target, content)
      }

      const wellknown = item.wellknown ?? {}
      const credentials = wellknown.credentials ?? []
      const lookups = wellknown.get ?? {}
      const entries = wellknown.entries ?? []
      const credential = (integrationID: string, value: LoadCredential) =>
        new Credential.Info({
          id: Credential.ID.create(),
          integrationID: Integration.ID.make(integrationID),
          label: "default",
          value: Credential.Key.make({
            type: "key",
            key: value.key ?? "secret",
            ...(value.metadata ? { metadata: value.metadata } : {}),
          }),
        })
      const credentialNode = makeGlobalNode({
        service: Credential.Service,
        layer: Layer.succeed(
          Credential.Service,
          Credential.Service.of({
            refresh: () => Effect.die("unused"),
            all: () => Effect.succeed([]),
            list: (integrationID) =>
              Effect.succeed(
                credentials
                  .filter((value) => value.integrationID === integrationID)
                  .map((value) => credential(value.integrationID, value)),
              ),
            get: (id) => {
              const value = lookups[id]
              return Effect.succeed(value ? credential("lookup", value) : undefined)
            },
            create: () => Effect.die("unused"),
            activate: () => Effect.die("unused"),
            update: () => Effect.die("unused"),
            remove: () => Effect.die("unused"),
          }),
        ),
        deps: [],
      })
      const wellknownNode = makeGlobalNode({
        service: WellKnown.Service,
        layer: Layer.succeed(
          WellKnown.Service,
          WellKnown.Service.of({
            entries: () => Effect.succeed(entries as readonly WellKnown.Entry[]),
            snapshot: () => entries as readonly WellKnown.Entry[],
            refresh: () => Effect.succeed(false),
            add: () => Effect.die("unused"),
            remove: () => Effect.die("unused"),
            resolve: () =>
              wellknown.resolveError
                ? Effect.fail(new Error("unavailable"))
                : Effect.succeed((wellknown.resolve ?? []) as WellKnown.Config[]),
            cached: () => Effect.succeed((wellknown.cached ?? []) as WellKnown.Config[]),
            clearCached: () => Effect.void,
          }),
        ),
        deps: [],
      })

      const globalConfig = path.join(root, item.global.config)
      const home = path.join(root, item.global.home)
      const directory = path.join(root, item.location.directory)
      const options: LoadOptions = { system: false, ...item.options }
      if (typeof options.system === "string") options.system = path.join(root, options.system)
      if (typeof options.file === "string") options.file = path.join(root, options.file)
      const locationLayer = Layer.succeed(
        Location.Service,
        Location.Service.of({
          directory: AbsolutePath.make(directory),
          workspaceID: undefined,
          project: {
            id: Project.ID.make(item.location.project_id),
            directory: AbsolutePath.make(directory),
            canonical: AbsolutePath.make(directory),
          },
        }),
      )
      const flags = { channel: item.flags?.channel ?? "unknown", overrides: item.flags?.overrides ?? null }
      const watcher = Watcher.testLayer
      process.env.OPENCODE_FLAGS = flags.overrides ?? ""
      const built = AppNodeBuilder.build(LayerNode.group([Config.node, Bus.node]), [
        Config.node.replace(Config.configured(options)),
        Location.node.replace(locationLayer),
        Global.node.replace(Global.layerWith({ config: globalConfig, home })),
        Credential.node.replace(credentialNode),
        WellKnown.node.replace(wellknownNode),
        App.node.replace(App.layer({ channel: flags.channel })),
        Watcher.node.replace(watcher),
      ])
      const program = Effect.gen(function* () {
        const config = yield* Config.Service
        for (const integration of item.integrations ?? []) {
          yield* config.integration!(
            Integration.ID.make(integration.integrationID),
            ConfigIntegration.make({
              integrationID: Integration.ID.make(integration.integrationID),
              connection: integration.connection as ConfigIntegration.Document["connection"],
              organization: integration.organization,
              info: integration.info,
            }),
          )
        }
        const loaded = (yield* config.entries()).filter(
          (entry) => entry.path === undefined || inside(entry.path),
        )
        const compatibility = yield* config.compatibility!()
        return {
          entries: loaded.map((entry) => ({
            wire: JSON.parse(JSON.stringify(encodeEntry(entry))),
            origin: entryOrigin(entry),
          })),
          compatibility,
          statements: ManagedPolicy.statements(loaded).map((statement) => ({
            action: statement.action,
            resource: statement.resource,
            effect: statement.effect,
            message: statement.message,
          })),
          decisions: (item.policy ?? []).map((entry) => ({
            action: entry.action,
            resource: entry.resource,
            effect: ManagedPolicy.decision(ManagedPolicy.statements(loaded), entry.action, entry.resource),
          })),
        }
      })
      console.log = () => {}
      const layer = Layer.provide(
        Layer.mergeAll(built, watcher),
        Layer.succeed(FlagResolve.Overrides, flags.overrides ?? ""),
      )
      const result = await Effect.runPromise(program.pipe(Effect.provide(layer)))
      const loaded = result.entries.map((entry) => {
        const wire = entry.wire as Record<string, unknown>
        if (typeof wire.path === "string") wire.path = rel(wire.path)
        return entry.origin ? { ...wire, origin: entry.origin } : wire
      })
      lines.push(
        `${JSON.stringify(name)}:${JSON.stringify({
          directories: item.directories ?? [],
          files: item.files ?? {},
          options: item.options ?? {},
          location: item.location,
          global: item.global,
          flags,
          integrations: item.integrations ?? [],
          wellknown: {
            entries,
            credentials,
            get: lookups,
            resolve: wellknown.resolve ?? null,
            resolveError: wellknown.resolveError ?? false,
            cached: wellknown.cached ?? [],
          },
          policy: item.policy ?? [],
          entries: loaded,
          compatibility: {
            claude: result.compatibility.claude.filter(inside).map(rel),
            agents: result.compatibility.agents.filter(inside).map(rel),
          },
          statements: result.statements,
          decisions: result.decisions,
        })}`,
      )
    } finally {
      console.log = log
      if (previousFlags === undefined) delete process.env.OPENCODE_FLAGS
      else process.env.OPENCODE_FLAGS = previousFlags
      await rm(root, { recursive: true, force: true })
    }
  }
  process.stdout.write(`{\n${lines.join(",\n")}\n}\n`)
}

type UpdateCase = {
  files?: Record<string, string>
  patch: { shell: string | null }
}

async function exportUpdate() {
  const corpus = process.env.OPENCODE_CORPUS_ROOT
  if (!corpus) {
    console.error("corpus: update needs OPENCODE_CORPUS_ROOT")
    process.exit(1)
  }

  const cases: Record<string, UpdateCase> = {
    "create-when-neither": { patch: { shell: "added" } },
    "json-only": { files: { "opencode.json": `{"shell":"old"}\n` }, patch: { shell: "new" } },
    "both-jsonc-wins": {
      files: { "opencode.json": `{"shell":"json"}\n`, "opencode.jsonc": `{"shell":"jsonc"}\n` },
      patch: { shell: "both" },
    },
    "jsonc-comments-and-trailing-comma": {
      files: { "opencode.jsonc": `// top comment\n{\n  // shell comment\n  "shell": "old",\n}\n` },
      patch: { shell: "commented" },
    },
    "jsonc-tabs": {
      files: { "opencode.jsonc": `{\n\t"shell": "old",\n\t"username": "user"\n}\n` },
      patch: { shell: "tabbed" },
    },
    crlf: {
      files: { "opencode.json": `{\r\n  "shell": "old",\r\n  "username": "user"\r\n}\r\n` },
      patch: { shell: "windows" },
    },
    "missing-trailing-newline": { files: { "opencode.json": `{"shell":"old"}` }, patch: { shell: "newline" } },
    "replace-shell-not-last": {
      files: { "opencode.json": `{"username": "user", "shell": "old"}\n` },
      patch: { shell: "later" },
    },
    "remove-shell": {
      files: { "opencode.json": `{"shell": "old", "username": "user"}\n` },
      patch: { shell: null },
    },
    "remove-absent-shell": { files: { "opencode.json": `{"username":"user"}\n` }, patch: { shell: null } },
    "empty-object": { files: { "opencode.json": `{}` }, patch: { shell: "empty" } },
    "empty-object-newline": { files: { "opencode.json": `{}\n` }, patch: { shell: "empty-newline" } },
    "comments-only": { files: { "opencode.jsonc": `// only a comment\n` }, patch: { shell: "comment-only" } },
    "empty-string-shell": { files: { "opencode.json": `{"shell":"old"}\n` }, patch: { shell: "" } },
    "unicode-shell": { files: { "opencode.json": `{"shell":"old"}\n` }, patch: { shell: "sh -c 'echo héllo'" } },
    "jsonc-array-neighbour": {
      files: { "opencode.jsonc": `{\n  "instructions": ["a.md"],\n  "shell": "old"\n}\n` },
      patch: { shell: "array" },
    },
    "remove-from-jsonc-comments": {
      files: { "opencode.jsonc": `{\n  // keep\n  "shell": "old",\n  "username": "u"\n}\n` },
      patch: { shell: null },
    },
    "crlf-remove": { files: { "opencode.json": `{\r\n  "shell": "old"\r\n}\r\n` }, patch: { shell: null } },
  }

  const encodeEntry = Schema.encodeSync(Entry)
  const log = console.log
  const lines: string[] = []

  for (const name of Object.keys(cases).sort()) {
    const item = cases[name]!
    const candidate = path.join(corpus, "update", name)
    await rm(candidate, { recursive: true, force: true })
    await mkdir(candidate, { recursive: true })
    const root = await realpath(candidate)
    const inside = (value: string) => insideRoot(root, value)
    const rel = (value: string) => relativeToRoot("update", name, root, value)
    const previousFlags = process.env.OPENCODE_FLAGS
    try {
      const home = path.join(root, "home")
      const globalConfig = path.join(root, "global")
      const directory = path.join(root, "project")
      await mkdir(home, { recursive: true })
      await mkdir(globalConfig, { recursive: true })
      await mkdir(directory, { recursive: true })
      for (const [file, content] of Object.entries(item.files ?? {})) {
        await writeFile(path.join(globalConfig, file), content)
      }

      const credentialNode = makeGlobalNode({
        service: Credential.Service,
        layer: Layer.succeed(
          Credential.Service,
          Credential.Service.of({
            refresh: () => Effect.die("unused"),
            all: () => Effect.succeed([]),
            list: () => Effect.succeed([]),
            get: () => Effect.succeed(undefined),
            create: () => Effect.die("unused"),
            activate: () => Effect.die("unused"),
            update: () => Effect.die("unused"),
            remove: () => Effect.die("unused"),
          }),
        ),
        deps: [],
      })
      const wellknownNode = makeGlobalNode({
        service: WellKnown.Service,
        layer: Layer.succeed(
          WellKnown.Service,
          WellKnown.Service.of({
            entries: () => Effect.succeed([]),
            snapshot: () => [],
            refresh: () => Effect.succeed(false),
            add: () => Effect.die("unused"),
            remove: () => Effect.die("unused"),
            resolve: () => Effect.succeed([]),
            cached: () => Effect.succeed([]),
            clearCached: () => Effect.void,
          }),
        ),
        deps: [],
      })

      const locationLayer = Layer.succeed(
        Location.Service,
        Location.Service.of({
          directory: AbsolutePath.make(directory),
          workspaceID: undefined,
          project: {
            id: Project.ID.make("global"),
            directory: AbsolutePath.make(directory),
            canonical: AbsolutePath.make(directory),
          },
        }),
      )
      const watcher = Watcher.testLayer
      process.env.OPENCODE_FLAGS = ""
      const built = AppNodeBuilder.build(LayerNode.group([Config.node, Bus.node]), [
        Config.node.replace(Config.configured({ system: false })),
        Location.node.replace(locationLayer),
        Global.node.replace(Global.layerWith({ config: globalConfig, home })),
        Credential.node.replace(credentialNode),
        WellKnown.node.replace(wellknownNode),
        App.node.replace(App.layer({ channel: "unknown" })),
        Watcher.node.replace(watcher),
      ])
      const program = Effect.gen(function* () {
        const config = yield* Config.Service
        const bus = yield* Bus.Service
        const before = yield* config.entries()
        const updated = yield* bus.subscribe(Event.Updated).pipe(
          Stream.take(1),
          Stream.runDrain,
          Effect.forkChild({ startImmediately: true }),
        )
        yield* Effect.yieldNow
        yield* config.update!({ shell: item.patch.shell })
        const event = yield* Fiber.join(updated).pipe(Effect.timeoutOption("10 seconds"))
        const after = yield* config.entries()
        if (Option.isNone(event) && !isDeepStrictEqual(before, after))
          return yield* Effect.fail(
            new Error(`corpus: update ${name} changed entries without publishing a config.updated event`),
          )
        return after
      })
      console.log = () => {}
      const layer = Layer.provide(Layer.mergeAll(built, watcher), Layer.succeed(FlagResolve.Overrides, ""))
      const loaded = await Effect.runPromise(program.pipe(Effect.provide(layer)))
      const entries = loaded
        .filter((entry) => entry.path === undefined || inside(entry.path))
        .map((entry) => {
          const wire = JSON.parse(JSON.stringify(encodeEntry(entry))) as Record<string, unknown>
          if (typeof wire.path === "string") wire.path = rel(wire.path)
          const origin = entryOrigin(entry)
          return origin ? { ...wire, origin } : wire
        })
      const written =
        (
          await Promise.all(
            ["opencode.json", "opencode.jsonc"].map(async (file) => {
              const target = path.join(globalConfig, file)
              return (await stat(target)
                .then((value) => value.isFile())
                .catch(() => false))
                ? target
                : undefined
            }),
          )
        )
          .filter((file): file is string => file !== undefined)
          .at(-1) ?? path.join(globalConfig, "opencode.jsonc")
      lines.push(
        `${JSON.stringify(name)}:${JSON.stringify({
          files: item.files ?? {},
          patch: item.patch,
          file: rel(written),
          text: await readFile(written, "utf8"),
          entries,
        })}`,
      )
    } finally {
      console.log = log
      if (previousFlags === undefined) delete process.env.OPENCODE_FLAGS
      else process.env.OPENCODE_FLAGS = previousFlags
      await rm(root, { recursive: true, force: true })
    }
  }
  process.stdout.write(`{\n${lines.join(",\n")}\n}\n`)
}

function entryOrigin(entry: Entry) {
  if (entry.type === "directory") return undefined
  if (entry instanceof ConfigIntegration.WellKnownDocument)
    return { kind: "wellknown" as const, integrationID: entry.integrationID }
  if (entry instanceof ConfigIntegration.Document)
    return {
      kind: "integration" as const,
      integrationID: entry.integrationID,
      connection: entry.connection,
      ...(entry.organization === undefined ? {} : { organization: entry.organization }),
    }
  if (entry instanceof ConfigManaged.Document) return { kind: "managed" as const }
  return { kind: "file" as const }
}
