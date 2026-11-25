import { Agent } from "@opencode/core/agent"
import { Bus } from "@opencode/core/bus"
import { Command } from "@opencode/core/command"
import { Config } from "@opencode/core/config"
import { ConfigAgentPlugin } from "@opencode/core/config/plugin/agent"
import { ConfigCommandPlugin } from "@opencode/core/config/plugin/command"
import { ConfigCompatibilityPlugin } from "@opencode/core/config/plugin/compatibility"
import { ConfigInstructionPlugin } from "@opencode/core/config/plugin/instruction"
import { ConfigReferencePlugin } from "@opencode/core/config/plugin/reference"
import { ConfigSkillPlugin } from "@opencode/core/config/plugin/skill"
import { ConfigMarkdown } from "@opencode/core/config/markdown"
import { AppNodeBuilder } from "@opencode/core/effect/app-node-builder"
import { Watcher } from "@opencode/core/filesystem/watcher"
import { Form } from "@opencode/core/form"
import { InstructionDiscovery } from "@opencode/core/instruction-discovery"
import { InstructionBuiltIns } from "@opencode/core/instructions/builtins"
import { Instructions } from "@opencode/core/instructions/index"
import { Job } from "@opencode/core/job"
import { Location } from "@opencode/core/location"
import { Mcp } from "@opencode/core/mcp/index"
import { Permission } from "@opencode/core/permission"
import { AgentPlugin } from "@opencode/core/plugin/agent"
import { CommandPlugin } from "@opencode/core/plugin/command"
import { PlanPlugin } from "@opencode/core/plugin/plan"
import { SkillPlugin } from "@opencode/core/plugin/skill"
import { Project } from "@opencode/core/project"
import { Reference } from "@opencode/core/reference"
import { ReferenceInstructions } from "@opencode/core/reference/instructions"
import { RepositoryCache } from "@opencode/core/repository-cache"
import { AbsolutePath } from "@opencode/core/schema"
import { Session } from "@opencode/core/session"
import { SessionExecution } from "@opencode/core/session/execution"
import { SubagentJob } from "@opencode/core/session/subagent-job"
import { ShellSelect } from "@opencode/core/shell/select"
import { Skill } from "@opencode/core/skill"
import { SkillDiscovery } from "@opencode/core/skill/discovery"
import { SkillInstructions } from "@opencode/core/skill/instructions"
import { Directory, Document, Info } from "@opencode/schema/config"
import { ConfigPolicy } from "@opencode/schema/config/policy"
import { SessionInbox } from "@opencode/schema/session-inbox"
import { SessionMessage } from "@opencode/schema/session-message"
import { canonicalJson } from "@opencode/core/util/canonical-json"
import { Wildcard } from "@opencode/core/util/wildcard"
import { FSUtil } from "@opencode/util/fs-util"
import { LayerNode } from "@opencode/util/effect/layer-node"
import { Global } from "@opencode/util/global"
import { AppProcess } from "@opencode/util/process"
import { DateTime, Effect, Layer, Logger, Schema } from "effect"
import { HttpClient, HttpClientResponse } from "effect/http"
import { mkdir, mkdtemp, realpath, rm, symlink, writeFile } from "node:fs/promises"
import { tmpdir } from "node:os"
import path from "node:path"
import { tempGlobalLayer } from "./fixture/global"
import { location } from "./fixture/location"
import { emptyConfigLayer, emptyMcpLayer, testLocationLayer } from "./fixture/mcp"
import { offlineModels } from "./fixture/models"
import { registerIntegrationPolicy, setOrganizationPolicy } from "./fixture/policy"
import { readInitial } from "./lib/instructions"
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
else if (area === "instructions") await exportInstructions()
else if (area === "skills") await exportSkills()
else if (area === "references") await exportReferences()
else if (area === "evaluate") exportEvaluate()
else if (area === "forms") exportForms()
else {
  console.error(`corpus: unknown area ${area ?? ""}`)
  process.exit(1)
}

function exportEvaluate() {
  const caseRules: Permission.Ruleset = [
    { action: "*", resource: "*", effect: "allow" },
    { action: "edit", resource: ".opencode/*", effect: "ask" },
    { action: "read", resource: "*.env", effect: "deny" },
    { action: "shell", resource: "git push*", effect: "ask" },
  ]
  const machineRules: Permission.Ruleset = [{ action: "read", resource: "**/work/*", effect: "allow" }]
  const cases: EvaluateCase[] = [
    {
      name: "wildcard-action-star",
      platform: "other",
      action: "read",
      resource: "src/app.ts",
      rules: [[{ action: "*", resource: "*", effect: "allow" }]],
    },
    {
      name: "wildcard-action-prefix",
      platform: "other",
      action: "shell",
      resource: "git status",
      rules: [[{ action: "shell*", resource: "git *", effect: "allow" }]],
    },
    {
      name: "wildcard-action-prefix-miss",
      platform: "other",
      action: "edit",
      resource: "src/app.ts",
      rules: [[{ action: "shell*", resource: "*", effect: "allow" }]],
    },
    {
      name: "wildcard-resource-question",
      platform: "other",
      action: "read",
      resource: "ab",
      rules: [[{ action: "read", resource: "a?", effect: "allow" }]],
    },
    {
      name: "wildcard-resource-question-long",
      platform: "other",
      action: "read",
      resource: "abc",
      rules: [[{ action: "read", resource: "a?", effect: "allow" }]],
    },
    {
      name: "last-rule-wins",
      platform: "other",
      action: "read",
      resource: "src/a",
      rules: [
        [{ action: "*", resource: "*", effect: "allow" }],
        [{ action: "read", resource: "src/*", effect: "deny" }],
      ],
    },
    {
      name: "last-rule-wins-rulesets",
      platform: "other",
      action: "shell",
      resource: "git push",
      rules: [
        [{ action: "shell", resource: "git*", effect: "allow" }],
        [{ action: "shell", resource: "git push", effect: "ask" }],
      ],
    },
    {
      name: "no-match-default",
      platform: "other",
      action: "edit",
      resource: "src/a",
      rules: [[{ action: "read", resource: "*", effect: "allow" }]],
    },
    {
      name: "windows-rule-slash",
      platform: "other",
      action: "external_directory",
      resource: "C:/Users/x/proj/src/*",
      rules: [[{ action: "external_directory", resource: "C:\\Users\\x\\proj\\*", effect: "allow" }]],
    },
    {
      name: "windows-rule-slash-miss",
      platform: "other",
      action: "external_directory",
      resource: "C:/Users/x/other/*",
      rules: [[{ action: "external_directory", resource: "C:\\Users\\x\\proj\\*", effect: "allow" }]],
    },
    {
      name: "windows-resource-backslash",
      platform: "other",
      action: "read",
      resource: "C:\\work\\a",
      rules: [[{ action: "read", resource: "C:/work/*", effect: "allow" }]],
    },
    {
      name: "windows-drive-wildcard",
      platform: "other",
      action: "read",
      resource: "C:/work/a",
      rules: [[{ action: "read", resource: "*", effect: "allow" }]],
    },
    {
      name: "windows-double-star",
      platform: "other",
      action: "read",
      resource: "C:/work/a",
      rules: [[{ action: "read", resource: "**/work/*", effect: "allow" }]],
    },
    {
      name: "machine-any",
      platform: "other",
      action: "read",
      resource: "gpu:/work/a",
      rules: [[{ action: "read", resource: "*", effect: "allow" }]],
    },
    {
      name: "machine-double-star",
      platform: "other",
      action: "read",
      resource: "gpu:/work/a",
      rules: [machineRules],
    },
    {
      name: "machine-scheme",
      platform: "other",
      action: "read",
      resource: "gpu:/work/a",
      rules: [[{ action: "read", resource: "gpu:/work/*", effect: "allow" }]],
    },
    {
      name: "machine-scheme-wildcard",
      platform: "other",
      action: "read",
      resource: "gpu:/work/a",
      rules: [[{ action: "read", resource: "gpu:*", effect: "allow" }]],
    },
    {
      name: "machine-scheme-miss",
      platform: "other",
      action: "read",
      resource: "cpu:/work/a",
      rules: [[{ action: "read", resource: "gpu:/work/*", effect: "allow" }]],
    },
    {
      name: "machine-local-miss",
      platform: "other",
      action: "read",
      resource: "/work/a",
      rules: [[{ action: "read", resource: "gpu:/work/*", effect: "allow" }]],
    },
    {
      name: "machine-name-case-macos",
      platform: "macos",
      action: "read",
      resource: "gpu:/work/a",
      rules: [[{ action: "read", resource: "GPU:/work/*", effect: "allow" }]],
    },
    {
      name: "machine-name-case-other",
      platform: "other",
      action: "read",
      resource: "gpu:/work/a",
      rules: [[{ action: "read", resource: "GPU:/work/*", effect: "allow" }]],
    },
    {
      name: "machine-bare-deny",
      platform: "other",
      action: "edit",
      resource: "gpu:/work/a",
      rules: [[{ action: "*", resource: "*", effect: "deny" }]],
    },
    {
      name: "machine-local-any",
      platform: "other",
      action: "read",
      resource: "/work/a",
      rules: [[{ action: "read", resource: "*", effect: "allow" }]],
    },
    {
      name: "machine-shell",
      platform: "other",
      action: "shell",
      resource: "gpu:/bin/x",
      rules: [[{ action: "shell", resource: "*", effect: "allow" }]],
    },
    {
      name: "shell-command-suffix",
      platform: "other",
      action: "shell",
      resource: "git push",
      rules: [[{ action: "shell", resource: "git push *", effect: "ask" }]],
    },
    {
      name: "shell-command-suffix-args",
      platform: "other",
      action: "shell",
      resource: "git push origin",
      rules: [[{ action: "shell", resource: "git push *", effect: "ask" }]],
    },
    {
      name: "env-file-deny",
      platform: "other",
      action: "read",
      resource: ".env",
      rules: [[{ action: "read", resource: "*.env", effect: "deny" }]],
    },
    {
      name: "env-machine-deny",
      platform: "other",
      action: "read",
      resource: "gpu:/work/.env",
      rules: [[{ action: "read", resource: "*.env", effect: "deny" }]],
    },
    {
      name: "case-edit-macos",
      platform: "macos",
      action: "edit",
      resource: ".OPENCODE/plugins/guard.ts",
      rules: [caseRules],
    },
    {
      name: "case-edit-other",
      platform: "other",
      action: "edit",
      resource: ".OPENCODE/plugins/guard.ts",
      rules: [caseRules],
    },
    {
      name: "case-read-macos",
      platform: "macos",
      action: "read",
      resource: "Secrets.ENV",
      rules: [caseRules],
    },
    {
      name: "case-read-other",
      platform: "other",
      action: "read",
      resource: "Secrets.ENV",
      rules: [caseRules],
    },
    {
      name: "case-shell-macos",
      platform: "macos",
      action: "shell",
      resource: "GIT PUSH origin dev",
      rules: [caseRules],
    },
    {
      name: "case-shell-other",
      platform: "other",
      action: "shell",
      resource: "GIT PUSH origin dev",
      rules: [caseRules],
    },
  ]
  const lines = cases.map((item) =>
    JSON.stringify({ ...item, rule: evaluateOn(item.platform, item.action, item.resource, item.rules) }),
  )
  process.stdout.write(`[\n${lines.join(",\n")}\n]\n`)
}

interface EvaluateCase {
  name: string
  platform: "macos" | "other"
  action: string
  resource: string
  rules: Permission.Ruleset[]
}

function evaluateOn(
  platform: "macos" | "other",
  action: string,
  resource: string,
  rules: Permission.Ruleset[],
): Permission.Rule {
  const ignoreCase = platform === "macos" && ["external_directory", "read", "edit"].includes(action)
  return (
    rules
      .flat()
      .findLast(
        (rule) => Wildcard.match(action, rule.action) && Wildcard.match(resource, rule.resource, ignoreCase),
      ) ?? { action, resource: "*", effect: "ask" }
  )
}

function exportForms() {
  const stringOptions = [
    { value: "dev", label: "Development" },
    { value: "prod", label: "Production" },
  ]
  const tagOptions = [
    { value: "go", label: "Go" },
    { value: "ts", label: "TypeScript" },
  ]
  const cases: FormCase[] = [
    {
      name: "fields-empty",
      fields: [],
    },
    {
      name: "fields-duplicate-key",
      fields: [
        { key: "name", type: "string" },
        { key: "name", type: "string" },
      ],
    },
    {
      name: "fields-duplicate-key-after-external",
      fields: [
        { key: "approve", type: "external", url: "https://example.com/approve" },
        { key: "approve", type: "string" },
      ],
    },
    {
      name: "fields-when-missing-field",
      fields: [{ key: "name", type: "string", when: [{ key: "missing", op: "eq", value: "x" }] }],
    },
    {
      name: "fields-when-later-field",
      fields: [
        { key: "name", type: "string", when: [{ key: "flag", op: "eq", value: true }] },
        { key: "flag", type: "boolean" },
      ],
    },
    {
      name: "fields-when-boolean-value-mismatch",
      fields: [
        { key: "flag", type: "boolean" },
        { key: "name", type: "string", when: [{ key: "flag", op: "eq", value: "yes" }] },
      ],
    },
    {
      name: "fields-when-number-value-mismatch",
      fields: [
        { key: "count", type: "number" },
        { key: "name", type: "string", when: [{ key: "count", op: "eq", value: "5" }] },
      ],
    },
    {
      name: "fields-when-integer-value-mismatch",
      fields: [
        { key: "count", type: "integer" },
        { key: "name", type: "string", when: [{ key: "count", op: "eq", value: true }] },
      ],
    },
    {
      name: "fields-when-string-value-mismatch",
      fields: [
        { key: "mode", type: "string" },
        { key: "name", type: "string", when: [{ key: "mode", op: "eq", value: 3 }] },
      ],
    },
    {
      name: "fields-when-string-option-outside",
      fields: [
        { key: "env", type: "string", options: stringOptions },
        { key: "name", type: "string", when: [{ key: "env", op: "eq", value: "staging" }] },
      ],
    },
    {
      name: "fields-when-string-option-custom",
      fields: [
        { key: "env", type: "string", options: stringOptions, custom: true },
        { key: "name", type: "string", when: [{ key: "env", op: "eq", value: "staging" }] },
      ],
    },
    {
      name: "fields-when-multiselect-option-outside",
      fields: [
        { key: "tags", type: "multiselect", options: tagOptions },
        { key: "notes", type: "string", when: [{ key: "tags", op: "eq", value: "rs" }] },
      ],
    },
    {
      name: "fields-when-multiselect-option-custom",
      fields: [
        { key: "tags", type: "multiselect", options: tagOptions, custom: true },
        { key: "notes", type: "string", when: [{ key: "tags", op: "eq", value: "rs" }] },
      ],
    },
    {
      name: "fields-when-chain-valid",
      fields: [
        { key: "flag", type: "boolean" },
        { key: "count", type: "number", when: [{ key: "flag", op: "eq", value: true }] },
        { key: "note", type: "string", when: [{ key: "count", op: "eq", value: 1 }] },
      ],
    },
    {
      name: "fields-when-multi-condition-valid",
      fields: [
        { key: "mode", type: "string", options: stringOptions },
        { key: "flag", type: "boolean" },
        {
          key: "name",
          type: "string",
          when: [
            { key: "mode", op: "eq", value: "dev" },
            { key: "flag", op: "eq", value: true },
          ],
        },
      ],
    },
    {
      name: "answer-unknown-key",
      fields: [{ key: "name", type: "string" }],
      answer: { name: "Ada", extra: "x" },
    },
    {
      name: "answer-missing-required",
      fields: [{ key: "name", type: "string", required: true }],
      answer: {},
    },
    {
      name: "answer-empty-required",
      fields: [{ key: "name", type: "string", required: true }],
      answer: { name: "" },
    },
    {
      name: "answer-optional-absent",
      fields: [{ key: "name", type: "string" }],
      answer: {},
    },
    {
      name: "answer-string-wrong-type",
      fields: [{ key: "name", type: "string" }],
      answer: { name: 5 },
    },
    {
      name: "answer-string-min-length",
      fields: [{ key: "name", type: "string", minLength: 3 }],
      answer: { name: "ab" },
    },
    {
      name: "answer-string-max-length",
      fields: [{ key: "name", type: "string", maxLength: 3 }],
      answer: { name: "abcd" },
    },
    {
      name: "answer-string-utf16-length",
      fields: [{ key: "name", type: "string", maxLength: 1 }],
      answer: { name: "😀" },
    },
    {
      name: "answer-pattern-match",
      fields: [{ key: "code", type: "string", pattern: "^[a-z]+$" }],
      answer: { code: "abc" },
    },
    {
      name: "answer-pattern-mismatch",
      fields: [{ key: "code", type: "string", pattern: "^[a-z]+$" }],
      answer: { code: "ABC" },
    },
    {
      name: "answer-pattern-invalid",
      fields: [{ key: "code", type: "string", pattern: "(" }],
      answer: { code: "abc" },
    },
    {
      name: "answer-email-valid",
      fields: [{ key: "email", type: "string", format: "email" }],
      answer: { email: "user@example.com" },
    },
    {
      name: "answer-email-invalid",
      fields: [{ key: "email", type: "string", format: "email" }],
      answer: { email: "user@example" },
    },
    {
      name: "answer-uri-https",
      fields: [{ key: "link", type: "string", format: "uri" }],
      answer: { link: "https://example.com/path" },
    },
    {
      name: "answer-uri-mailto",
      fields: [{ key: "link", type: "string", format: "uri" }],
      answer: { link: "mailto:user@example.com" },
    },
    {
      name: "answer-uri-relative",
      fields: [{ key: "link", type: "string", format: "uri" }],
      answer: { link: "relative/path" },
    },
    {
      name: "answer-uri-invalid",
      fields: [{ key: "link", type: "string", format: "uri" }],
      answer: { link: "://invalid" },
    },
    {
      name: "answer-date-valid",
      fields: [{ key: "day", type: "string", format: "date" }],
      answer: { day: "2024-02-29" },
    },
    {
      name: "answer-date-not-leap",
      fields: [{ key: "day", type: "string", format: "date" }],
      answer: { day: "2023-02-29" },
    },
    {
      name: "answer-date-single-digit",
      fields: [{ key: "day", type: "string", format: "date" }],
      answer: { day: "2024-2-29" },
    },
    {
      name: "answer-datetime-valid",
      fields: [{ key: "at", type: "string", format: "date-time" }],
      answer: { at: "2024-02-29T10:30:00Z" },
    },
    {
      name: "answer-datetime-rollover",
      fields: [{ key: "at", type: "string", format: "date-time" }],
      answer: { at: "2024-02-30" },
    },
    {
      name: "answer-datetime-month-out-of-range",
      fields: [{ key: "at", type: "string", format: "date-time" }],
      answer: { at: "2024-13-01" },
    },
    {
      name: "answer-datetime-garbage",
      fields: [{ key: "at", type: "string", format: "date-time" }],
      answer: { at: "not-a-date" },
    },
    {
      name: "answer-datetime-midnight-24",
      fields: [{ key: "at", type: "string", format: "date-time" }],
      answer: { at: "2024-02-29T24:00:00Z" },
    },
    {
      name: "answer-datetime-midnight-24-seconds",
      fields: [{ key: "at", type: "string", format: "date-time" }],
      answer: { at: "2024-02-29T24:00:01Z" },
    },
    {
      name: "answer-string-option-invalid",
      fields: [{ key: "env", type: "string", options: stringOptions }],
      answer: { env: "staging" },
    },
    {
      name: "answer-string-option-valid",
      fields: [{ key: "env", type: "string", options: stringOptions }],
      answer: { env: "dev" },
    },
    {
      name: "answer-string-option-custom",
      fields: [{ key: "env", type: "string", options: stringOptions, custom: true }],
      answer: { env: "staging" },
    },
    {
      name: "answer-number-valid",
      fields: [{ key: "count", type: "number" }],
      answer: { count: 5 },
    },
    {
      name: "answer-number-wrong-type",
      fields: [{ key: "count", type: "number" }],
      answer: { count: "5" },
    },
    {
      name: "answer-number-minimum",
      fields: [{ key: "count", type: "number", minimum: 10 }],
      answer: { count: 5 },
    },
    {
      name: "answer-number-maximum",
      fields: [{ key: "count", type: "number", maximum: 10 }],
      answer: { count: 11 },
    },
    {
      name: "answer-integer-valid",
      fields: [{ key: "count", type: "integer" }],
      answer: { count: 5 },
    },
    {
      name: "answer-integer-fraction",
      fields: [{ key: "count", type: "integer" }],
      answer: { count: 1.5 },
    },
    {
      name: "answer-boolean-wrong-type",
      fields: [{ key: "enabled", type: "boolean" }],
      answer: { enabled: "yes" },
    },
    {
      name: "answer-multiselect-wrong-type",
      fields: [{ key: "tags", type: "multiselect", options: tagOptions }],
      answer: { tags: "go" },
    },
    {
      name: "answer-multiselect-required-empty",
      fields: [{ key: "tags", type: "multiselect", options: tagOptions, required: true }],
      answer: { tags: [] },
    },
    {
      name: "answer-multiselect-min-items",
      fields: [{ key: "tags", type: "multiselect", options: tagOptions, minItems: 2 }],
      answer: { tags: ["go"] },
    },
    {
      name: "answer-multiselect-max-items",
      fields: [{ key: "tags", type: "multiselect", options: tagOptions, maxItems: 1 }],
      answer: { tags: ["go", "ts"] },
    },
    {
      name: "answer-multiselect-option-invalid",
      fields: [{ key: "tags", type: "multiselect", options: tagOptions }],
      answer: { tags: ["rs"] },
    },
    {
      name: "answer-multiselect-option-custom",
      fields: [{ key: "tags", type: "multiselect", options: tagOptions, custom: true }],
      answer: { tags: ["rs"] },
    },
    {
      name: "answer-external-absent",
      fields: [{ key: "approve", type: "external", url: "https://example.com/approve" }],
      answer: {},
    },
    {
      name: "answer-external-false",
      fields: [{ key: "approve", type: "external", url: "https://example.com/approve" }],
      answer: { approve: false },
    },
    {
      name: "answer-external-string",
      fields: [{ key: "approve", type: "external", url: "https://example.com/approve" }],
      answer: { approve: "yes" },
    },
    {
      name: "answer-external-true",
      fields: [
        { key: "approve", type: "external", url: "https://example.com/approve" },
        { key: "name", type: "string", required: true },
      ],
      answer: { approve: true, name: "ok" },
    },
    {
      name: "answer-when-gated-required-satisfied",
      fields: [
        { key: "flag", type: "boolean" },
        { key: "name", type: "string", required: true, when: [{ key: "flag", op: "eq", value: true }] },
      ],
      answer: { flag: true, name: "ok" },
    },
    {
      name: "answer-when-inactive-answered",
      fields: [
        { key: "flag", type: "boolean" },
        { key: "name", type: "string", required: true, when: [{ key: "flag", op: "eq", value: true }] },
      ],
      answer: { flag: false, name: "ok" },
    },
    {
      name: "answer-when-active-required-missing",
      fields: [
        { key: "flag", type: "boolean" },
        { key: "name", type: "string", required: true, when: [{ key: "flag", op: "eq", value: true }] },
      ],
      answer: { flag: true },
    },
    {
      name: "answer-when-cascade-hidden-answered",
      fields: [
        { key: "flag", type: "boolean" },
        { key: "name", type: "string", required: true, when: [{ key: "flag", op: "eq", value: true }] },
        { key: "note", type: "string", when: [{ key: "name", op: "eq", value: "x" }] },
      ],
      answer: { flag: false, note: "hello" },
    },
    {
      name: "answer-when-neq-multiselect-included",
      fields: [
        { key: "tags", type: "multiselect", options: tagOptions },
        { key: "notes", type: "string", required: true, when: [{ key: "tags", op: "neq", value: "ts" }] },
      ],
      answer: { tags: ["ts"] },
    },
    {
      name: "answer-when-neq-multiselect-empty",
      fields: [
        { key: "tags", type: "multiselect", options: tagOptions },
        { key: "notes", type: "string", required: true, when: [{ key: "tags", op: "neq", value: "ts" }] },
      ],
      answer: { tags: [] },
    },
    {
      name: "answer-when-neq-multiselect-other",
      fields: [
        { key: "tags", type: "multiselect", options: tagOptions },
        { key: "notes", type: "string", required: true, when: [{ key: "tags", op: "neq", value: "ts" }] },
      ],
      answer: { tags: ["go"] },
    },
    {
      name: "answer-when-eq-multiselect-multi",
      fields: [
        { key: "tags", type: "multiselect", options: tagOptions },
        { key: "notes", type: "string", required: true, when: [{ key: "tags", op: "eq", value: "go" }] },
      ],
      answer: { tags: ["go", "ts"], notes: "ok" },
    },
    {
      name: "answer-when-empty-condition-active",
      fields: [{ key: "name", type: "string", required: true, when: [] }],
      answer: {},
    },
  ]
  const lines = cases.map((item) =>
    JSON.stringify({
      name: item.name,
      fields: item.fields,
      ...(item.answer === undefined ? {} : { answer: item.answer }),
      validateFields: Form.validateFields(item.fields) ?? null,
      ...(item.answer === undefined ? {} : { validateAnswer: Form.validateAnswer(item.fields, item.answer) ?? null }),
    }),
  )
  process.stdout.write(`[\n${lines.join(",\n")}\n]\n`)
}

interface FormCase {
  name: string
  fields: ReadonlyArray<Form.Field>
  answer?: Form.Answer
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
    "quoted-colon": '---\ndescription: "a: b"\n---\nbody',
    "single-quoted-colon": "---\ndescription: 'a: b'\n---\nbody",
    "url-colon": "---\nurl: http://example.com/a\n---\nbody",
    "flow-with-comment": "---\ntools: [read, edit] # comment: with colon\n---\nbody",
    "key-with-hyphen": "---\nmy-key: a: b\n---\nbody",
    "indented-colon": "---\nnested:\n  value: a: b\n---\nbody",
    "comment-inside":
      "---\ndescription: Review changes. Keywords: review, audit\n# comment: kept\nmodel: { providerID: example }\n---\nbody",
    "empty-value": "---\ndescription:\nmodel: { providerID: example }\n---\nbody",
    "braced-tag": "---\nsummary: {Team} Review changes\n---\nbody",
    "trailing-open-brace": "---\nsummary: {Team Review changes\n---\nbody",
    "four-dashes": "----\ntitle: Hi\n---\nbody",
    bom: "\uFEFF---\ntitle: Hi\n---\nbody",
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
  return AppNodeBuilder.build(LayerNode.group([Agent.node, Bus.node, FSUtil.node, Location.node, Global.node]), [
    Global.node.replace(globalLayer),
    Location.node.replace(locationLayer),
  ])
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
        "commands/structured.md": "---\nmodel: { providerID: example, model: chat, variant: high }\n---\nStructured",
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
      commands: Object.fromEntries(cases.map((item, index) => [`case${index}`, { template: item.template }])),
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

type InstructionOptions = {
  project?: boolean
  global?: boolean
  claude?: boolean
}

type InstructionCase = {
  name: string
  home: string
  config: string
  project: string
  directory: string
  options?: InstructionOptions
  files: Record<string, string>
  instructions?: string[]
}

function instructionDocument(instructions: string[]) {
  return new Document({ type: "document", info: new Info({ instructions }) })
}

function instructionRespond(handler: (url: string) => Response) {
  return HttpClient.make((request) => Effect.succeed(HttpClientResponse.fromWeb(request, handler(request.url))))
}

function instructionLayer(input: {
  config: string
  home: string
  locationServiceLayer: Layer.Layer<Location.Service>
  instructions?: string[]
  project?: boolean
  global?: boolean
  claude?: boolean
}) {
  const watcher = Watcher.testLayer
  const config = Config.testLayer(input.instructions ? [instructionDocument(input.instructions)] : [])
  return Layer.mergeAll(
    AppNodeBuilder.build(
      LayerNode.group([
        InstructionDiscovery.node,
        Bus.node,
        Config.node,
        FSUtil.node,
        Global.node,
        Location.node,
        Watcher.node,
      ]),
      [
        Config.node.replace(config),
        InstructionDiscovery.node.replace(
          InstructionDiscovery.configured({
            project: input.project,
            global: input.global,
            claude: input.claude,
          }),
        ),
        Global.node.replace(Global.layerWith({ config: input.config, home: input.home })),
        Location.node.replace(input.locationServiceLayer),
        Watcher.node.replace(watcher),
      ],
    ),
    watcher,
    config,
    Layer.succeed(
      HttpClient.HttpClient,
      instructionRespond(() => new Response(null, { status: 404 })),
    ),
  )
}

function environmentLayer(locationServiceLayer: Layer.Layer<Location.Service>) {
  return AppNodeBuilder.build(LayerNode.group([InstructionBuiltIns.node, Global.node, Location.node]), [
    Global.node.replace(Global.layerWith({ tmp: "/tmp/opencode" })),
    Location.node.replace(locationServiceLayer),
  ])
}

async function exportInstructions() {
  const cases: InstructionCase[] = [
    {
      name: "global-and-upward",
      config: "global",
      home: "home",
      project: "home/code/repo",
      directory: "home/code/repo/packages/core",
      files: {
        "AGENTS.md": "outside",
        "global/AGENTS.md": "global",
        "home/code/AGENTS.md": "shared",
        "home/code/repo/AGENTS.md": "project",
        "home/code/repo/packages/core/AGENTS.md": "package",
      },
    },
    {
      name: "claude-fallback",
      config: "global",
      home: "home",
      project: "home/repo",
      directory: "home/repo/packages/core",
      options: { claude: true },
      files: {
        "home/.claude/CLAUDE.md": "claude global",
        "home/repo/AGENTS.md": "project agents",
        "home/repo/CLAUDE.md": "project claude",
        "home/repo/packages/core/CLAUDE.md": "package claude",
      },
    },
    {
      name: "claude-disabled",
      config: "global",
      home: "home",
      project: "home/repo",
      directory: "home/repo/packages/core",
      options: { claude: false },
      files: {
        "home/.claude/CLAUDE.md": "claude global",
        "home/repo/AGENTS.md": "project agents",
        "home/repo/CLAUDE.md": "project claude",
        "home/repo/packages/core/CLAUDE.md": "package claude",
      },
    },
    {
      name: "project-outside-home",
      config: "global",
      home: "home",
      project: "scratch/repo",
      directory: "scratch/repo/packages/core",
      files: {
        "AGENTS.md": "outside",
        "scratch/repo/AGENTS.md": "project",
      },
    },
    {
      name: "configured-sources",
      config: "global",
      home: "home",
      project: "repo",
      directory: "repo/packages/core",
      instructions: ["rules/*.md", "{{root}}/absolute.md", "~/home.md", "~/notes/**/*.md", "missing.md"],
      files: {
        "repo/AGENTS.md": "agents",
        "repo/rules/a.md": "first",
        "repo/rules/b.md": "second",
        "absolute.md": "absolute",
        "home/home.md": "home",
        "home/notes/guide.md": "note top",
        "home/notes/projects/memory.md": "note nested",
      },
    },
    {
      name: "empty-agents",
      config: "global",
      home: "home",
      project: "repo",
      directory: "repo",
      files: { "repo/AGENTS.md": "" },
    },
    {
      name: "nested-project",
      config: "global",
      home: "home",
      project: "home/code/repo",
      directory: "home/code/repo/a/b",
      files: {
        "home/code/AGENTS.md": "shared",
        "home/code/repo/AGENTS.md": "project",
        "home/code/repo/a/b/AGENTS.md": "nested",
      },
    },
  ]
  const output: string[] = []
  for (const item of cases) output.push(await exportInstructionCase(item))
  output.push(await exportEnvironmentCase({ name: "environment-git", vcs: "git" }))
  output.push(await exportEnvironmentCase({ name: "environment-none", vcs: null }))
  output.push(JSON.stringify(exportCanonical()))
  process.stdout.write(`[\n${output.join(",\n")}\n]\n`)
}

async function exportInstructionCase(item: InstructionCase) {
  const root = await mkdtemp(path.join(tmpdir(), "opencode-instruction-case-"))
  try {
    const canonical = await realpath(root)
    const absolute = (relative: string) => path.join(canonical, relative)
    await mkdir(absolute(item.directory), { recursive: true })
    await mkdir(absolute(item.home), { recursive: true })
    for (const [file, content] of Object.entries(item.files)) {
      const target = absolute(file)
      await mkdir(path.dirname(target), { recursive: true })
      await writeFile(target, content)
    }
    const instructions = item.instructions?.map((source) => source.replace("{{root}}", canonical))
    const locationServiceLayer = Layer.succeed(
      Location.Service,
      Location.Service.of(
        location(
          { directory: AbsolutePath.make(absolute(item.directory)) },
          { projectDirectory: AbsolutePath.make(absolute(item.project)) },
        ),
      ),
    )
    const program = Effect.gen(function* () {
      yield* ConfigInstructionPlugin.Plugin.effect(host())
      const discovery = yield* InstructionDiscovery.Service
      const listed = yield* discovery.list()
      const initial = Array.isArray(listed) ? yield* readInitial(yield* discovery.load()) : { text: "" }
      return {
        kind: "discovery",
        name: item.name,
        home: absolute(item.home),
        config: absolute(item.config),
        project: absolute(item.project),
        directory: absolute(item.directory),
        vcs: null,
        options: {
          project: item.options?.project ?? true,
          global: item.options?.global ?? true,
          claude: item.options?.claude ?? true,
        },
        files: item.files,
        instructions: instructions ?? [],
        list: Array.isArray(listed) ? listed : "unavailable",
        text: initial.text,
      }
    })
    const result = await Effect.runPromise(
      program.pipe(
        Effect.scoped,
        Effect.provide(
          instructionLayer({
            config: absolute(item.config),
            home: absolute(item.home),
            instructions,
            project: item.options?.project,
            global: item.options?.global,
            claude: item.options?.claude,
            locationServiceLayer,
          }),
        ),
        Effect.provide(Logger.layer([])),
      ),
    )
    return JSON.stringify(result).replaceAll(JSON.stringify(canonical).slice(1, -1), "{{root}}")
  } finally {
    await rm(root, { recursive: true, force: true })
  }
}

async function exportEnvironmentCase(item: { name: string; vcs: "git" | null }) {
  const directory = "/repo/packages/core"
  const projectDirectory = "/repo"
  const locationServiceLayer = Layer.succeed(
    Location.Service,
    Location.Service.of(
      location(
        { directory: AbsolutePath.make(directory) },
        {
          projectDirectory: AbsolutePath.make(projectDirectory),
          ...(item.vcs === "git" ? { vcs: { type: "git", store: AbsolutePath.make("/repo/.git") } } : {}),
        },
      ),
    ),
  )
  const program = Effect.gen(function* () {
    const builtins = yield* InstructionBuiltIns.Service
    const list = yield* builtins.load()
    const observed = yield* Instructions.read(list)
    const values: Record<string, Schema.Json> = {}
    let raw = ""
    for (const entry of observed) {
      if (typeof entry.value !== "string") continue
      values[entry.key] = entry.value
      if (entry.key === "core/environment") raw = entry.value
    }
    const text = Instructions.renderInitial(list, values)
      .filter((part) => part.key === "core/environment")
      .map((part) => part.text)
      .join("\n\n")
    return {
      kind: "environment",
      name: item.name,
      environment: { directory, projectDirectory, vcs: item.vcs, tmp: "/tmp/opencode" },
      raw,
      text,
    }
  })
  const result = await Effect.runPromise(
    program.pipe(
      Effect.scoped,
      Effect.provide(environmentLayer(locationServiceLayer)),
      Effect.provide(Logger.layer([])),
    ),
  )
  return JSON.stringify(result)
}

function exportCanonical() {
  const values: Array<Schema.Json> = [
    null,
    true,
    "text",
    0,
    -0,
    1.5,
    1e21,
    [],
    {},
    { b: 2, a: 1, c: { z: 1, y: [3, 2, 1] } },
    { a: 'line\nquote"tab\t', u: "é" },
  ]
  return {
    kind: "canonical",
    name: "values",
    values,
    canonical: values.map((value) => canonicalJson(value)),
    hashes: values.map((value) => Instructions.hash(value)),
  }
}

type SkillCase = {
  name: string
  files?: Record<string, string>
  directories?: string[]
  documents?: Array<Record<string, unknown>>
  integration?: Record<string, unknown>
  compatibility?: { claude: string[]; agents: string[] }
  policies?: Array<ConfigPolicy.Info>
  home: string
  directory: string
  app: { version: string; channel: string }
}

function scrubSkillDiagnostics(content: string) {
  const prefixes = ["- opencode version: ", "- install/channel: ", "- OS: ", "- Terminal: ", "- Shell: "]
  return content
    .split("\n")
    .map((line) => {
      const prefix = prefixes.find((item) => line.startsWith(item))
      return prefix === undefined ? line : `${prefix}<value>`
    })
    .join("\n")
}

function skillLayer() {
  return Layer.merge(
    AppNodeBuilder.build(LayerNode.group([Skill.node, SkillInstructions.node, Bus.node, FSUtil.node])),
    Watcher.testLayer,
  )
}

async function exportSkills() {
  const app = { version: "1.2.3", channel: "beta" }
  const localPolicies: Array<ConfigPolicy.Info> = [{ action: "skill.use", resource: "*", effect: "allow" }]
  const organizationPolicies: Array<ConfigPolicy.Info> = [
    { action: "skill.use", resource: "*", effect: "deny" },
    { action: "skill.use", resource: "opencode", effect: "allow" },
  ]
  const cases: SkillCase[] = [
    {
      name: "builtins",
      documents: [
        {
          plugins: ["package-plugin", "-disabled", "local.ts", { package: "package-plugin", options: { enabled: true } }],
        },
      ],
      home: "home",
      directory: "project",
      app,
    },
    {
      name: "config-sources",
      files: {
        "opencode/skill/deploy/SKILL.md": `---
name: Deploy
description: Ship it
---
# Deploy`,
        "opencode/skills/opencode/SKILL.md": `---
name: Config OpenCode
description: Configured override
---
# Config OpenCode`,
        "opencode/skills/loose.md": `---
name: Loose
description: Loose skill
---
# Loose`,
        "opencode/skills/node_modules/skip/SKILL.md": `---
name: Skipped
description: Must not load
---
# Skipped`,
        "opencode/skills/.hidden/SKILL.md": `---
name: Hidden
description: Must not load
---
# Hidden`,
        "opencode/skills/a/b/c/d/e/f/g/SKILL.md": `---
name: Deep
description: Too deep
---
# Deep`,
        "home/shared/manual/SKILL.md": `---
name: Manual
description: Manual only
metadata:
  opencode/autoinvoke: false
---
# Manual`,
        "project/relative/tool.md": `---
name: Tool
description: Tool skill
---
# Tool`,
        "project/relative/nested/SKILL.md": `---
name: Nested
---
# Nested`,
        "project/relative/broken.md": "---\ndescription: foo: bar\nmetadata: [\n---\n# broken",
      },
      directories: ["opencode"],
      documents: [{ skills: ["~/shared", "./relative"] }],
      home: "home",
      directory: "project",
      app,
    },
    {
      name: "compatibility",
      files: {
        "claude/skills/review/SKILL.md": `---
name: Review
description: Claude review
---
# Review`,
        "claude/skills/loose.md": `---
name: Claude Loose
description: Claude loose
---
# Claude Loose`,
        "agents/skills/check/SKILL.md": `---
name: Check
description: Agent check
---
# Check`,
      },
      compatibility: { claude: ["claude"], agents: ["agents"] },
      home: "home",
      directory: "project",
      app,
    },
    {
      name: "policy",
      files: {
        "opencode/skills/team/SKILL.md": `---
name: Team
description: Team skill
---
# Team`,
        "opencode/skills/deploy/SKILL.md": `---
name: Deploy
description: Deploy skill
---
# Deploy`,
      },
      directories: ["opencode"],
      documents: [{ policies: localPolicies }],
      integration: { policies: organizationPolicies },
      policies: organizationPolicies,
      home: "home",
      directory: "project",
      app,
    },
  ]
  const output: string[] = []
  for (const item of cases) output.push(await exportSkillCase(item))
  process.stdout.write(`[\n${output.join(",\n")}\n]\n`)
}

async function exportSkillCase(item: SkillCase) {
  const root = await mkdtemp(path.join(tmpdir(), "opencode-skill-case-"))
  try {
    const canonical = await realpath(root)
    const absolute = (relative: string) => path.join(canonical, relative)
    await mkdir(absolute(item.home), { recursive: true })
    await mkdir(absolute(item.directory), { recursive: true })
    for (const [file, content] of Object.entries(item.files ?? {})) {
      const target = absolute(file)
      await mkdir(path.dirname(target), { recursive: true })
      await writeFile(target, content)
    }
    const entries: Array<Directory | Document> = [
      ...(item.directories ?? []).map(
        (directory) => new Directory({ type: "directory", path: AbsolutePath.make(absolute(directory)) }),
      ),
      ...(item.documents ?? []).map(
        (document) => new Document({ type: "document", info: decodeConfig(document) }),
      ),
    ]
    const compatibility = item.compatibility && {
      claude: item.compatibility.claude.map((directory) => AbsolutePath.make(absolute(directory))),
      agents: item.compatibility.agents.map((directory) => AbsolutePath.make(absolute(directory))),
    }
    const program = Effect.gen(function* () {
      const service = yield* Skill.Service
      const pluginHost = host({
        app: { name: "test", ...item.app },
        skill: {
          list: () => Effect.die("unused skill.list"),
          transform: service.transform,
          reload: service.reload,
        },
      })
      yield* SkillPlugin.Plugin.effect(pluginHost)
      if (compatibility) yield* ConfigCompatibilityPlugin.Plugin.effect(pluginHost)
      yield* ConfigSkillPlugin.Plugin.effect(pluginHost).pipe(
        Effect.provideService(SkillDiscovery.Service, SkillDiscovery.Service.of({ pull: () => Effect.succeed([]) })),
        Effect.provideService(Global.Service, Global.Service.of({ ...Global.make(), home: absolute(item.home) })),
        Effect.provideService(
          Location.Service,
          Location.Service.of(location({ directory: AbsolutePath.make(absolute(item.directory)) })),
        ),
      )
      if (item.policies) {
        yield* setOrganizationPolicy(item.policies)
        yield* registerIntegrationPolicy({ skill: service, events: (yield* Bus.Service).subscribe() })
      }
      const list = (yield* service.list()).map((skill: Skill.Info) =>
        skill.id === "report" ? { ...skill, content: scrubSkillDiagnostics(skill.content) } : skill,
      )
      const instructions = yield* SkillInstructions.Service
      const guidance = yield* instructions.load([]).pipe(Effect.flatMap(readInitial))
      return {
        name: item.name,
        files: item.files ?? {},
        directories: item.directories ?? [],
        documents: item.documents ?? [],
        ...(item.integration === undefined ? {} : { integration: item.integration }),
        ...(compatibility === undefined ? {} : { compatibility: item.compatibility }),
        home: item.home,
        directory: item.directory,
        app: item.app,
        list,
        guidance: guidance.text,
      }
    })
    const result = await Effect.runPromise(
      program.pipe(
        Effect.scoped,
        Effect.provide(skillLayer()),
        Effect.provide(Config.testLayer(entries, compatibility)),
        Effect.provide(Logger.layer([])),
      ),
    )
    console.error(
      `skills ${item.name}: ${result.list.map((skill: Skill.Info) => skill.id).join(", ")} (guidance ${result.guidance.split("\n").length} lines)`,
    )
    return JSON.stringify(result).replaceAll(JSON.stringify(canonical).slice(1, -1), "{{root}}")
  } finally {
    await rm(root, { recursive: true, force: true })
  }
}

type ReferenceCase = {
  name: string
  documents: Array<Record<string, unknown>>
}

function referenceLayer(input: { home: string; repos: string }) {
  const globalLayer = Layer.succeed(
    Global.Service,
    Global.Service.of(Global.make({ home: input.home, repos: input.repos })),
  )
  const cache = Layer.mock(RepositoryCache.Service, {
    ensure: () => Effect.die("unexpected Git materialization"),
  })
  return AppNodeBuilder.build(LayerNode.group([Reference.node, ReferenceInstructions.node, Bus.node]), [
    Global.node.replace(globalLayer),
    RepositoryCache.node.replace(cache),
  ])
}

async function exportReferences() {
  const cases: ReferenceCase[] = [
    {
      name: "local-sources",
      documents: [
        {
          references: {
            docs: "./docs",
            shared: "~/shared",
            notes: { path: "notes", description: "Notes for the team", hidden: true },
            plain: "./plain",
          },
        },
      ],
    },
    {
      name: "git-sources",
      documents: [
        {
          references: {
            sdk: "owner/repo",
            branch: {
              repository: "gitlab.com/group/repo",
              branch: "feature/x",
              description: "Branch docs",
              hidden: true,
            },
            scp: "git@github.com:owner/scp.git",
            port: "ssh://git@example.com:2222/owner/repo",
            url: "https://github.com/owner/url.git",
            invalid: "invalid",
            "invalid-branch": { repository: "owner/repo", branch: "../escape" },
            file: "file:///docs",
          },
        },
      ],
    },
    {
      name: "invalid-aliases",
      documents: [
        {
          references: {
            "": "owner/repo",
            "bad/alias": "owner/repo",
            "bad alias": "owner/repo",
            "bad,alias": "owner/repo",
            "bad`alias": "owner/repo",
            ok: "owner/repo",
          },
        },
      ],
    },
    {
      name: "later-documents-win",
      documents: [
        { references: { docs: "./first" } },
        { references: { docs: { path: "./second", description: "Second docs" } } },
      ],
    },
  ]
  const output: string[] = []
  for (const item of cases) output.push(await exportReferenceCase(item))
  process.stdout.write(`[\n${output.join(",\n")}\n]\n`)
}

async function exportReferenceCase(item: ReferenceCase) {
  const root = await mkdtemp(path.join(tmpdir(), "opencode-reference-case-"))
  try {
    const canonical = await realpath(root)
    const absolute = (relative: string) => path.join(canonical, relative)
    const home = absolute("home")
    const directory = absolute("project")
    const repos = absolute("repos")
    await mkdir(home, { recursive: true })
    await mkdir(directory, { recursive: true })
    const entries: Array<Document> = item.documents.map(
      (document) => new Document({ type: "document", info: decodeConfig(document) }),
    )
    const program = Effect.gen(function* () {
      const service = yield* Reference.Service
      yield* ConfigReferencePlugin.Plugin.effect(
        host({ reference: { list: service.list, transform: service.transform, reload: service.reload } }),
      ).pipe(
        Effect.provideService(Global.Service, Global.Service.of(Global.make({ home, repos }))),
        Effect.provideService(
          Location.Service,
          Location.Service.of(location({ directory: AbsolutePath.make(directory) })),
        ),
      )
      const list = yield* service.list()
      const instructions = yield* ReferenceInstructions.Service
      const guidance = yield* instructions.load().pipe(Effect.flatMap(readInitial))
      return {
        name: item.name,
        home,
        directory,
        repos,
        documents: item.documents,
        list,
        guidance: guidance.text,
      }
    })
    const result = await Effect.runPromise(
      program.pipe(
        Effect.scoped,
        Effect.provide(referenceLayer({ home, repos })),
        Effect.provide(Config.testLayer(entries)),
        Effect.provide(Logger.layer([])),
      ),
    )
    console.error(
      `references ${item.name}: ${result.list.map((reference: Reference.Info) => reference.name).join(", ")}`,
    )
    return JSON.stringify(result)
      .replaceAll(JSON.stringify(repos).slice(1, -1), "{{repos}}")
      .replaceAll(JSON.stringify(canonical).slice(1, -1), "{{root}}")
  } finally {
    await rm(root, { recursive: true, force: true })
  }
}
