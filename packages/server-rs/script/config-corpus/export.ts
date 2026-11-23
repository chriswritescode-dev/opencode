import { ConfigExperimental } from "@opencode/schema/config/experimental"
import { Document, Info } from "@opencode/schema/config"
import { FlagResolve } from "@opencode/core/flag/resolve"
import { ConfigNormalize } from "@opencode/core/config/normalize"
import { ConfigVariable } from "@opencode/core/config/variable"
import { FSUtil } from "@opencode/util/fs-util"
import { LayerNode } from "@opencode/util/effect/layer-node"
import { Effect, Logger, Option, Schema } from "effect"
import { parse, type ParseError } from "jsonc-parser"
import path from "node:path"
import { mkdir, rm, writeFile } from "node:fs/promises"

const options = { errors: "all", onExcessProperty: "ignore" } as const
const decodeInfo = Schema.decodeUnknownOption(Info, options)

const area = process.argv[2]
if (area === "flags") exportFlags()
else if (area === "normalize") exportNormalize()
else if (area === "substitute") await exportSubstitute()
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

function exportNormalize() {
  const cases: Record<string, { group: "current" | "v1"; text: string }> = {
    "agents-invalid-color": {
      group: "current",
      text: JSON.stringify({ agents: { reviewer: { color: "blue", system: "Keep" } } }),
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
    "v1-agents-precedence": {
      group: "v1",
      text: JSON.stringify({
        agent: { reviewer: { prompt: "agent" }, agentOnly: { prompt: "agent-only" } },
        mode: { reviewer: { prompt: "mode" }, modeOnly: { prompt: "mode-only" } },
        agents: { reviewer: { system: "native" }, nativeOnly: { system: "native-only" } },
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

  function run(item: { group: "current" | "v1"; text: string }) {
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
