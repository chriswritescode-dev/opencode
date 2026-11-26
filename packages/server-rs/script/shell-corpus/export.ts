import { chmodSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from "node:fs"
import path from "node:path"
import { ShellScan } from "@opencode/core/shell/scan"
import { ShellSelect } from "@opencode/core/shell/select"

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

if (area === "select") exportSelect()
else if (area === "scan-bash") exportScanBash()
else if (area === "scan-powershell") exportScanPowerShell()
else {
  console.error(`corpus: unknown area ${area ?? ""}`)
  process.exit(1)
}
