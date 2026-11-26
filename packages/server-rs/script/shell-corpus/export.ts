import { chmodSync, mkdirSync, writeFileSync } from "node:fs"
import path from "node:path"
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

if (area === "select") exportSelect()
else {
  console.error(`corpus: unknown area ${area ?? ""}`)
  process.exit(1)
}
