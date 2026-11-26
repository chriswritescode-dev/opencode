import * as bunTest from "bun:test"
import { appendFileSync } from "node:fs"
import * as real from "../src/shell/scan.ts"

const realScan = real.scan
const realPowerShell = real.scanPowerShell
const output = process.env.OPENCODE_SHELL_CORPUS_OUT ?? ""
const powerShellMode = process.env.OPENCODE_SHELL_CORPUS_POWERSHELL === "1"

const source = () => (new Error().stack ?? "").match(/test\/shell-scan\/[^/\n]+\.test\.ts:\d+/)?.[0] ?? "unknown"

const scan = (input: string, dialect: "bash" | "zsh" | "posix" = "posix") => {
  if (!powerShellMode) appendFileSync(output, `${JSON.stringify({ input, dialect, source: source() })}\n`)
  return realScan(input, dialect)
}

const scanPowerShell = (input: string) => {
  if (powerShellMode) appendFileSync(output, `${JSON.stringify({ input, source: source() })}\n`)
  return realPowerShell(input)
}

type Passthrough = ((...args: unknown[]) => Passthrough) & { [key: string]: Passthrough }

const passthrough: Passthrough = new Proxy(function () {} as unknown as Passthrough, {
  get: () => passthrough,
  apply: () => passthrough,
})

bunTest.mock.module("bun:test", () => ({
  ...bunTest,
  expect: passthrough as unknown as typeof bunTest.expect,
}))
bunTest.mock.module("../src/shell/scan.ts", () => ({
  scan,
  scanPowerShell,
  ShellScan: { scan, scanPowerShell },
}))
if (!powerShellMode) {
  Reflect.set(Bun, "which", () => "/bin/true")
  Reflect.set(Bun, "spawnSync", () => ({
    exitCode: 0,
    stdout: Buffer.from(""),
    stderr: Buffer.from(""),
    success: true,
  }))
}
