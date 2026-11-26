import { chmodSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import fs from "node:fs"
import os from "node:os"
import path from "node:path"
import { gzipSync } from "node:zlib"
import { ShellParse } from "@opencode/core/shell/parse"
import { ShellPermissions } from "@opencode/core/shell/permissions"
import { ShellResult } from "@opencode/core/shell/result"
import { ShellScan } from "@opencode/core/shell/scan"
import { ShellSelect } from "@opencode/core/shell/select"
import type { FileAccess } from "@opencode/core/file-access"
import { Effect } from "effect"

const area = process.argv[2]

type Case = Record<string, unknown>

function exportSelect() {
  const root = path.resolve(process.env.OPENCODE_CORPUS_ROOT ?? "")
  const home = process.env.HOME ?? ""
  const bin = path.join(root, "bin")
  const global = path.join(root, "global")
  const empty = path.join(root, "empty")
  for (const directory of [bin, global, empty]) mkdirSync(directory, { recursive: true })

  const names = ["bash", "zsh", "sh", "dash", "fish", "nu", "pwsh", "powershell", "cmd", "cmd.exe"]
  for (const name of names) executable(path.join(bin, name))
  executable(path.join(global, "bash"))

  const cases: Case[] = []
  const display = (value?: string | null) =>
    value === null || value === undefined ? null : value.replaceAll(home, "{{home}}").replaceAll(root, "{{root}}")

  function addResolve(
    name: string,
    priority: "config" | "compat",
    options: { shell?: string; configured?: string; path?: string; bin?: string } = {},
  ) {
    process.env.PATH = options.path ?? ""
    process.env.SHELL = options.shell ?? ""
    ShellSelect.resolve.reset()
    const expected = ShellSelect.resolve({ priority }, options.configured, undefined, options.bin)
    cases.push({
      name,
      kind: "resolve",
      priority,
      configured: display(options.configured),
      shell: display(options.shell),
      path: display(options.path) ?? "",
      bin: display(options.bin),
      expected: display(expected),
    })
  }

  const plain = (name: string, value: Case) =>
    cases.push({ ...value, name, ...(typeof value.shell === "string" ? { shell: display(value.shell) } : {}) })

  for (const name of names) {
    const file = path.join(bin, name)
    addResolve(`resolve-config-${name}`, "config", { shell: file, path: bin })
    addResolve(`resolve-compat-${name}`, "compat", { shell: file, path: bin })
    plain(`login-${name}`, { kind: "login", shell: file, expected: ShellSelect.login(file) })
    plain(`ps-${name}`, { kind: "ps", shell: file, expected: ShellSelect.ps(file) })
    plain(`name-${name}`, { kind: "name", shell: file, expected: ShellSelect.name(file) })
    plain(`args-${name}`, {
      kind: "args",
      shell: file,
      command: "echo *",
      expected: ShellSelect.args(file, "echo *"),
    })
  }

  const namePaths = [
    "/usr/bin/bash",
    "/usr/bin/env bash",
    "/bin/zsh",
    "/opt/homebrew/bin/pwsh",
    "C:\\Windows\\System32\\cmd.exe",
    "C:\\Program Files\\PowerShell\\7\\pwsh.exe",
    "C:/Windows/System32/WindowsPowerShell/v1.0/powershell.exe",
    ".\\bash.exe",
    "bash.exe",
    "powershell.exe",
    "nu.exe",
  ]
  for (const file of namePaths) plain(`name-${file}`, { kind: "name", shell: file, expected: ShellSelect.name(file) })

  const argsPaths = [
    ["/bin/bash", "printf '%s\\n' hi"],
    ["/bin/zsh", "echo *"],
    ["/bin/dash", "echo hi"],
    ["/bin/sh", "echo hi"],
    ["/usr/bin/fish", "echo hi"],
    ["/usr/bin/nu", "echo hi"],
    ["/usr/bin/env bash", "echo hi"],
    ["pwsh", "Get-Location"],
    ["powershell", "Get-Location"],
    ["cmd", "dir"],
    ["C:\\Windows\\System32\\cmd.exe", "dir"],
    ["C:\\Program Files\\PowerShell\\7\\pwsh.exe", "Get-Location"],
  ] as const
  for (const [file, command] of argsPaths)
    plain(`args-${file}`, { kind: "args", shell: file, command, expected: ShellSelect.args(file, command) })

  addResolve("resolve-config-directory-bash", "config", { configured: path.join(bin, "bash"), path: bin })
  addResolve("resolve-config-denied-fish", "config", { configured: path.join(bin, "fish"), path: bin })
  addResolve("resolve-compat-denied-fish", "compat", { configured: path.join(bin, "fish"), path: bin })
  addResolve("resolve-compat-denied-nu", "compat", { configured: path.join(bin, "nu"), path: bin })
  addResolve("resolve-config-missing-path", "config", { configured: path.join(root, "missing"), path: bin })
  addResolve("resolve-config-bare-name", "config", { configured: "zsh", path: bin })
  addResolve("resolve-config-windows-path", "config", {
    configured: "C:\\Windows\\System32\\cmd.exe",
    path: bin,
  })
  addResolve("resolve-config-missing-without-bash", "config", { configured: path.join(root, "missing"), path: empty })
  addResolve("resolve-environment-dash", "config", { shell: path.join(bin, "dash"), path: bin })
  addResolve("resolve-environment-fish", "config", { shell: path.join(bin, "fish"), path: bin })
  addResolve("resolve-environment-fish-compat", "compat", { shell: path.join(bin, "fish"), path: bin })
  addResolve("resolve-environment-unset", "config", { path: bin })
  addResolve("resolve-environment-unset-compat", "compat", { path: bin })
  addResolve("resolve-environment-unset-without-bash", "config", { path: empty })
  addResolve("resolve-environment-unset-from-bin", "config", { path: empty, bin: global })
  addResolve("resolve-environment-absolute-without-bash", "config", { shell: path.join(bin, "zsh"), path: empty })
  addResolve("resolve-environment-missing", "config", { shell: path.join(root, "missing"), path: bin })

  console.log(`__SHELL_CORPUS__${JSON.stringify(cases)}`)
}

function executable(file: string) {
  writeFileSync(file, "")
  chmodSync(file, 0o755)
}

function recordScan(area: "scan-bash" | "scan-powershell", runs: { names: string[]; extra?: string[] }[]) {
  const root = process.env.OPENCODE_CORPUS_ROOT ?? ""
  const calls = path.join(root, "scan-calls.jsonl")
  rmSync(calls, { force: true })

  for (const run of runs) {
    const result = Bun.spawnSync(
      [
        "bun",
        "test",
        ...run.names.map((name) => `test/shell-scan/${name}.test.ts`),
        "--preload",
        "./test/opencode-rs-shell-corpus-record.ts",
        ...(run.extra ?? []),
      ],
      {
        env: {
          ...process.env,
          OPENCODE_SHELL_CORPUS_OUT: calls,
          ...(area === "scan-powershell" ? { OPENCODE_SHELL_CORPUS_POWERSHELL: "1" } : {}),
        },
        stdout: "ignore",
        stderr: "inherit",
      },
    )
    if (result.exitCode !== 0) {
      console.error(`corpus: scanning ${area} fixtures failed`)
      process.exit(1)
    }
  }

  if (!existsSync(calls)) {
    console.error("corpus: scanning recorded no fixtures")
    process.exit(1)
  }
  return { calls, expected: runs.flatMap((run) => run.names) }
}

function readScanCalls(calls: string) {
  const contributors = new Set<string>()
  const callsList = readFileSync(calls, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((line) => {
      const call = JSON.parse(line) as { input: string; dialect?: ShellScan.Dialect; source: string }
      contributors.add(call.source.split(":")[0] ?? "")
      return call
    })
  return { callsList, contributors }
}

function requireContributors(area: string, expected: string[], contributors: Set<string>) {
  for (const name of expected)
    if (!contributors.has(`test/shell-scan/${name}.test.ts`)) {
      console.error(`corpus: ${area} ${name}.test.ts contributed no fixtures`)
      process.exit(1)
    }
}

function exportScanBash() {
  const { calls, expected } = recordScan("scan-bash", [
    {
      names: [
        "scan",
        "bash-syntax",
        "bash-safety",
        "adversarial",
        "arithmetic",
        "closure",
        "compound-parity",
        "generated",
        "mutation",
        "parity-regressions",
        "redirect",
      ],
    },
    { names: ["oracle"], extra: ["-t", "reports or rejects"] },
  ])
  const { callsList, contributors } = readScanCalls(calls)
  const cases: Case[] = callsList.map((call, index) => ({
    name: `scan-bash-${index + 1}`,
    source: call.source,
    dialect: call.dialect,
    input: call.input,
    expected: ShellScan.scan(call.input, call.dialect as ShellScan.Dialect),
  }))
  requireContributors("scan-bash", expected, contributors)
  console.log(`__SHELL_CORPUS__${JSON.stringify(cases)}`)
}

function exportScanPowerShell() {
  const { calls, expected } = recordScan("scan-powershell", [
    { names: ["powershell-syntax", "powershell-safety", "powershell-runtime"] },
  ])
  const { callsList, contributors } = readScanCalls(calls)
  const cases: Case[] = callsList.map((call, index) => ({
    name: `scan-powershell-${index + 1}`,
    source: call.source,
    input: call.input,
    expected: ShellScan.scanPowerShell(call.input),
  }))
  requireContributors("scan-powershell", expected, contributors)
  console.log(`__SHELL_CORPUS__${JSON.stringify(cases)}`)
}


const FIXED_HOME = "/corpus/home"
const FIXED_PASSWD = [
  "root:x:0:0:root:/root:/bin/sh",
  "nobody:x:65534:65534:nobody:/nonexistent:/usr/sbin/nologin",
  "corpus:x:1000:1000:corpus:/corpus/home:/bin/sh",
].join("\n")
const FIXED_USER = "corpus"

const passwdTable = Object.fromEntries(
  FIXED_PASSWD.split("\n").flatMap((line) => {
    const fields = line.split(":")
    return fields[0] && fields[5] ? [[fields[0], fields[5]]] : []
  }),
)

function patchParseEnvironment() {
  const original = fs.readFileSync
  Reflect.set(fs, "readFileSync", (file: string, options?: unknown) =>
    file === "/etc/passwd" ? FIXED_PASSWD : original(file, options as never),
  )
  Reflect.set(os, "homedir", () => FIXED_HOME)
  Reflect.set(os, "userInfo", () => ({ username: FIXED_USER, homedir: FIXED_HOME, uid: 0, gid: 0, shell: null }))
  process.env.HOME = FIXED_HOME
  process.env.USERPROFILE = FIXED_HOME
}

function parseEnvironment(command: string) {
  const keys = new Set(["HOME", "USERPROFILE"])
  for (const match of command.matchAll(/\$\{env:([^}]+)\}/gi)) keys.add(match[1] ?? "")
  for (const match of command.matchAll(/\$env:([A-Za-z_][A-Za-z0-9_]*)/gi)) keys.add(match[1] ?? "")
  return Object.fromEntries([...keys].flatMap((key) => (process.env[key] === undefined ? [] : [[key, process.env[key]]])))
}

function exportParse() {
  const root = process.env.OPENCODE_CORPUS_ROOT ?? ""
  const calls = path.join(root, "parse-calls.jsonl")
  rmSync(calls, { force: true })
  const result = Bun.spawnSync(
    [
      "bun",
      "test",
      "test/shell-parse.test.ts",
      "test/shell-parse-compatibility.test.ts",
      "test/shell-parse-syntax.test.ts",
      "--preload",
      "./test/opencode-rs-shell-corpus-parse.ts",
    ],
    {
      env: {
        ...process.env,
        HOME: FIXED_HOME,
        USERPROFILE: FIXED_HOME,
        OPENCODE_PARSE_HOME: FIXED_HOME,
        OPENCODE_PARSE_USER: FIXED_USER,
        OPENCODE_PARSE_PASSWD: FIXED_PASSWD,
        OPENCODE_PARSE_CORPUS_OUT: calls,
      },
      stdout: "ignore",
      stderr: "inherit",
    },
  )
  if (result.exitCode !== 0) {
    console.error("corpus: recording parse fixtures failed")
    process.exit(1)
  }
  if (!existsSync(calls)) {
    console.error("corpus: recording parse fixtures recorded nothing")
    process.exit(1)
  }
  patchParseEnvironment()
  const cases: Case[] = readFileSync(calls, "utf8")
    .split("\n")
    .filter(Boolean)
    .map((line, index) => ({ ...(JSON.parse(line) as Case), name: `parse-${index + 1}` }))

  for (const manual of [
    { name: "parse-env-bash-env", shell: "bash", command: "git status", env: { BASH_ENV: "/startup" } },
    { name: "parse-env-bash-function", shell: "bash", command: "git status", env: { "BASH_FUNC_cd%%": "() { :; }" } },
    { name: "parse-env-home-expansion", shell: "bash", command: "cd $HOME; pwd", env: { HOME: "/session-home" } },
    { name: "parse-env-home-bare", shell: "bash", command: "cd; pwd", env: { HOME: "/session-home" } },
    { name: "parse-env-cdpath-command", shell: "bash", command: "CDPATH=/outside cd child", env: {} },
    { name: "parse-env-cdpath", shell: "bash", command: "cd child; pwd", env: { CDPATH: "/outside" } },
    {
      name: "parse-env-powershell-home",
      shell: "pwsh",
      command: "Set-Location $HOME; Set-Location $PWD; Set-Location $target",
      env: { HOME: "/session-home" },
    },
    {
      name: "parse-env-powershell-var",
      shell: "pwsh",
      command: "Set-Item Env:T /outside; Set-Location $env:T",
      env: { T: "/workspace" },
    },
    { name: "parse-commandless-redirect", shell: "bash", command: "> output", env: {} },
    { name: "parse-commandless-append", shell: "bash", command: "2>> output", env: {} },
    { name: "parse-assignment-only", shell: "bash", command: "FOO=bar", env: {} },
  ]) {
    const previous: Record<string, string | undefined> = {}
    for (const [key, value] of Object.entries(manual.env)) {
      previous[key] = process.env[key]
      process.env[key] = value
    }
    const scanned = Effect.runSync(Effect.result(ShellParse.scan(manual.command, manual.shell, "/workspace")))
    const recordedEnvironment = parseEnvironment(manual.command)
    for (const [key, value] of Object.entries(previous)) {
      if (value === undefined) delete process.env[key]
      else process.env[key] = value
    }
    const record = {
      name: manual.name,
      source: "test/shell-parse-compatibility.test.ts:manual",
      command: manual.command,
      shell: manual.shell,
      cwd: "/workspace",
      tildeHome: FIXED_HOME,
      systemHome: FIXED_HOME,
      environment: recordedEnvironment,
      passwd: passwdTable,
      currentUser: { name: FIXED_USER, home: FIXED_HOME },
    }
    cases.push(
      scanned._tag === "Success"
        ? { ...record, result: scanned.success }
        : { ...record, error: scanned.failure.reason },
    )
  }
  console.log(`__SHELL_CORPUS__${JSON.stringify(cases)}`)
}

function exportParseParity() {
  const commands = generated()
  if (commands.length <= 20_000) {
    console.error(`corpus: parse parity generated only ${commands.length} commands`)
    process.exit(1)
  }
  const lines: string[] = []
  for (const [shell, command] of commands) {
    const parsed = Effect.runSync(ShellParse.scan(command, shell, "/workspace"))
    lines.push(JSON.stringify({ command, shell, expected: parsed }))
  }
  const compressed = gzipSync(Buffer.from(`${lines.join("\n")}\n`), { level: 9 })
  compressed.writeUInt32LE(0, 4)
  console.log(`__SHELL_CORPUS__${JSON.stringify({ base64: compressed.toString("base64") })}`)
}

type ResolveTarget = {
  absolute: string
  resource: string
  externalDirectory?: { action: "external_directory"; directory: string; resource: string; save: string }
}

const internalTarget = (absolute: string): ResolveTarget => ({ absolute, resource: "." })
const externalTarget = (directory: string): ResolveTarget => ({
  absolute: directory,
  resource: directory,
  externalDirectory: {
    action: "external_directory",
    directory,
    resource: path.join(directory, "*"),
    save: path.join(directory, "*"),
  },
})

function exportPermissions() {
  const cases = [
    {
      name: "permissions-scanned",
      command: "git status && npm run test -- --watch",
      program: "/bin/bash",
      once: false,
      target: internalTarget("/workspace"),
      resolved: {},
      rules: [],
    },
    {
      name: "permissions-external-directory",
      command: "cd /outside && git status",
      program: "/bin/bash",
      once: false,
      target: internalTarget("/workspace"),
      resolved: { "/outside": externalTarget("/outside") },
      rules: [],
    },
    {
      name: "permissions-directory-only",
      command: "cd /outside",
      program: "/bin/bash",
      once: false,
      target: internalTarget("/workspace"),
      resolved: { "/outside": externalTarget("/outside") },
      rules: [],
    },
    {
      name: "permissions-directory-only-once",
      command: "cd /outside",
      program: "/bin/bash",
      once: true,
      target: internalTarget("/workspace"),
      resolved: { "/outside": externalTarget("/outside") },
      rules: [],
    },
    {
      name: "permissions-scanned-once",
      command: "git status && npm run test -- --watch",
      program: "/bin/bash",
      once: true,
      target: internalTarget("/workspace"),
      resolved: {},
      rules: [],
    },
    {
      name: "permissions-no-command",
      command: "FOO=bar",
      program: "/bin/bash",
      once: false,
      target: internalTarget("/workspace"),
      resolved: {},
      rules: [],
    },
    {
      name: "permissions-external-target",
      command: "git status",
      program: "/bin/bash",
      once: false,
      target: externalTarget("/outside"),
      resolved: {},
      rules: [],
    },
    {
      name: "permissions-external-deduplicated",
      command: "cd /outside && pwd",
      program: "/bin/bash",
      once: false,
      target: externalTarget("/outside"),
      resolved: { "/outside": externalTarget("/outside") },
      rules: [],
    },
    {
      name: "permissions-opaque",
      command: 'echo "',
      program: "/bin/bash",
      once: false,
      target: internalTarget("/workspace"),
      resolved: {},
      rules: [],
    },
    {
      name: "permissions-opaque-deny",
      command: 'echo "',
      program: "/bin/bash",
      once: false,
      target: internalTarget("/workspace"),
      resolved: {},
      rules: [{ action: "*", resource: "*", effect: "deny" }],
    },
    {
      name: "permissions-opaque-other-deny",
      command: 'echo "',
      program: "/bin/bash",
      once: false,
      target: internalTarget("/workspace"),
      resolved: {},
      rules: [{ action: "git *", resource: "*", effect: "deny" }],
    },
  ] as const

  const exported: Case[] = []
  for (const item of cases) {
    const access = {
      resolve: (input: { path: string }) => Effect.succeed((item.resolved as Record<string, ResolveTarget>)[input.path]),
    } as unknown as FileAccess.Interface
    const result = Effect.runSync(
      Effect.result(
        ShellPermissions.requests(access, {
          command: item.command,
          program: item.program,
          target: item.target as never,
          once: item.once,
          rules: Effect.succeed([...item.rules]),
        }),
      ),
    )
    if (result._tag === "Success") exported.push({ ...item, expected: result.success })
    else exported.push({ ...item, error: result.failure.reason })
  }
  console.log(`__SHELL_CORPUS__${JSON.stringify(exported)}`)
}

function exportResult() {
  const info = (status: string, extra: Record<string, unknown> = {}) => ({
    id: "sh_result",
    status,
    command: "work",
    file: "/tmp/sh_result.out",
    ...extra,
  })
  const captured = (output: string, truncated = false, killed?: string) => ({
    output,
    truncated,
    ...(killed === undefined ? {} : { killed }),
  })
  const cases: Case[] = []

  const add = (name: string, kind: string, input: Record<string, unknown>, expected: unknown) =>
    cases.push({ name, kind, ...input, expected })

  add(
    "result-output-missing",
    "output",
    { result: { info: info("killed"), capture: null } },
    ShellResult.output({ info: info("killed"), capture: undefined } as never),
  )
  add(
    "result-output-complete",
    "output",
    {
      result: {
        info: info("exited", { exit: 0, signal: "SIGTERM" }),
        capture: captured("done\n", false, "Command was killed: the process daemon stopped"),
      },
    },
    ShellResult.output({
      info: info("exited", { exit: 0, signal: "SIGTERM" }),
      capture: captured("done\n", false, "Command was killed: the process daemon stopped"),
    } as never),
  )
  add(
    "result-output-timeout",
    "output",
    { result: { info: info("timeout", { signal: "SIGKILL" }), capture: captured("waiting\n", true) } },
    ShellResult.output({
      info: info("timeout", { signal: "SIGKILL" }),
      capture: captured("waiting\n", true),
    } as never),
  )

  const notices = [
    ["result-notice-none", { output: "", truncated: false }, undefined],
    ["result-notice-exit-zero", { output: "", truncated: false, exit: 0 }, undefined],
    ["result-notice-exit", { output: "", truncated: false, exit: 7 }, 5],
    ["result-notice-signal", { output: "", truncated: false, signal: "SIGKILL" }, undefined],
    ["result-notice-unknown-signal", { output: "", truncated: false, signal: "SIGMADEUP" }, undefined],
    ["result-notice-timeout", { output: "", truncated: false, exit: 7, timeout: true }, 2_500],
    ["result-notice-timeout-zero", { output: "", truncated: false, exit: 7, timeout: true }, 0],
    ["result-notice-killed", { output: "", truncated: false, killed: "Command was killed" }, 9_000],
  ] as const
  for (const [name, output, timeout] of notices)
    add(name, "notice", { output, timeout }, ShellResult.notice(output as never, timeout) ?? null)

  const interrupted = [
    ["result-interrupted-capture", { info: info("killed"), capture: captured("partial\n") }],
    ["result-interrupted-truncated", { info: info("killed"), capture: captured("tail\n", true) }],
    ["result-interrupted-empty", { info: info("running"), capture: captured("(no output)") }],
    ["result-interrupted-missing", { info: info("killed"), capture: null }],
  ] as const
  for (const [name, result] of interrupted)
    add(name, "interrupted", { result }, ShellResult.interrupted(result as never))

  for (const [name, result] of interrupted)
    add(name.replace("interrupted", "metadata"), "metadata", { result }, ShellResult.metadata(ShellResult.output(result as never)))

  add(
    "result-notification",
    "notification",
    {
      input: {
        shellID: "sh_result",
        jobID: "job_1",
        command: "work",
        state: "completed",
        text: "done",
        output: { output: "done\n", truncated: false, exit: 0 },
      },
    },
    ShellResult.notification({
      shellID: "sh_result",
      jobID: "job_1",
      command: "work",
      state: "completed",
      text: "done",
      output: { output: "done\n", truncated: false, exit: 0 },
    } as never),
  )
  add(
    "result-user-notification-done",
    "userNotification",
    { result: { info: info("exited", { exit: 0 }), capture: captured("done\n") } },
    ShellResult.userNotification({ info: info("exited", { exit: 0 }), capture: captured("done\n") } as never),
  )
  add(
    "result-user-notification-failed",
    "userNotification",
    { result: { info: info("exited", { exit: 7 }), capture: captured("bad\n") } },
    ShellResult.userNotification({ info: info("exited", { exit: 7 }), capture: captured("bad\n") } as never),
  )
  add(
    "result-user-notification-cancelled",
    "userNotification",
    { result: { info: info("killed"), capture: captured("") } },
    ShellResult.userNotification({ info: info("killed"), capture: captured("") } as never),
  )
  console.log(`__SHELL_CORPUS__${JSON.stringify(cases)}`)
}

function generated() {
  const result: Array<[shell: string, command: string]> = []
  const bashHeads = ["git", "npm", "echo", "printf", "cat", "cd"]
  const bashArgs = [
    "",
    " status",
    " plain",
    " 'two words'",
    ' "two words"',
    " escaped\\ space",
    " hash#word",
    " --flag=value",
    " ./relative",
    " /tmp/absolute",
  ]
  const bashSeparators = [" ; ", " && ", " || ", " | ", " |& ", "\n"]
  const redirects = ["", " > output", " 2> error", " < input", " >> output"]
  for (const head of bashHeads)
    for (const arg of bashArgs)
      for (const assignment of ["", "X=value ", "X='two words' ", 'X="two words" '])
        for (const redirect of redirects) result.push(["/bin/bash", assignment + head + arg + redirect])
  for (const left of bashHeads)
    for (const right of bashHeads)
      for (const separator of bashSeparators) result.push(["/bin/bash", `${left} left${separator}${right} right`])
  for (const outer of ["echo", "printf", "cat"])
    for (const inner of bashHeads) {
      result.push(["/bin/bash", `${outer} $(${inner} nested)`])
      result.push(["/bin/bash", `${outer} "$(${inner} nested)"`])
      result.push(["/bin/bash", `${outer} pre$(${inner} nested)post`])
      result.push(["/bin/bash", `${outer} \`${inner} nested\``])
    }

  const powershellHeads = ["Get-ChildItem", "Write-Output", "Test-Path", "Remove-Item", "Set-Location"]
  const powershellArgs = ["", " value", " 'two words'", ' "two words"', " -Path C:\\tmp", " -LiteralPath '..\\outside'"]
  const powershellSeparators = [";", "|", "&&", "||", "\n", "\r\n"]
  for (const head of powershellHeads) for (const arg of powershellArgs) result.push(["pwsh", head + arg])
  for (const left of powershellHeads)
    for (const right of powershellHeads)
      for (const separator of powershellSeparators) result.push(["pwsh", `${left} left${separator}${right} right`])

  let state = 0x5eed1234
  const random = (length: number) => {
    state = (Math.imul(state, 1664525) + 1013904223) >>> 0
    return state % length
  }
  for (let index = 0; index < 10_000; index++) {
    const left = bashHeads[random(bashHeads.length)]
    const right = bashHeads[random(bashHeads.length)]
    const arg = bashArgs[random(bashArgs.length)]
    const separator = bashSeparators[random(bashSeparators.length)]
    const bashForms = [
      `${left}${arg}${separator}${right} fuzz${index}`,
      `echo $(${right} fuzz${index})`,
      `${left}${arg} # ignored\n${right} fuzz${index}`,
      `X=value ${left}${arg}${redirects[random(redirects.length)]}`,
      `${left} 'two words'${separator}${right} fuzz${index}`,
    ]
    result.push(["/bin/bash", bashForms[index % bashForms.length]])

    const powershellLeft = powershellHeads[random(powershellHeads.length)]
    const powershellRight = powershellHeads[random(powershellHeads.length)]
    const powershellArg = powershellArgs[random(powershellArgs.length)]
    const powershellSeparator = powershellSeparators[random(powershellSeparators.length)]
    const powershellForms = [
      `${powershellLeft}${powershellArg}${powershellSeparator}${powershellRight} fuzz${index}`,
      `${powershellLeft}${powershellArg} # ignored\n${powershellRight} fuzz${index}`,
      `${powershellLeft} fuzz${index} > output; ${powershellRight}${powershellArg}`,
      `${powershellLeft} "fuzz${index}" | ${powershellRight}${powershellArg}`,
    ]
    result.push(["pwsh", powershellForms[index % powershellForms.length]])
  }
  return result
}

if (area === "select") exportSelect()
else if (area === "scan-bash") exportScanBash()
else if (area === "scan-powershell") exportScanPowerShell()
else if (area === "parse") exportParse()
else if (area === "parse-parity") exportParseParity()
else if (area === "permissions") exportPermissions()
else if (area === "result") exportResult()
else {
  console.error(`corpus: unknown area ${area ?? ""}`)
  process.exit(1)
}
