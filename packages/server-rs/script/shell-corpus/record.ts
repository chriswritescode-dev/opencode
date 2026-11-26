import * as bunTest from "bun:test"
import { appendFileSync } from "node:fs"
import * as real from "../src/shell/scan.ts"

const realScan = real.scan
const realPowerShell = real.scanPowerShell
const output = process.env.OPENCODE_SHELL_CORPUS_OUT ?? ""

const scan = (input: string, dialect: "bash" | "zsh" | "posix" = "posix") => {
  const stack = new Error().stack ?? ""
  const source = stack.match(/test\/shell-scan\/[^/\n]+\.test\.ts:\d+/)?.[0] ?? "unknown"
  appendFileSync(output, `${JSON.stringify({ input, dialect, source })}\n`)
  return realScan(input, dialect)
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
  scanPowerShell: realPowerShell,
  ShellScan: { scan, scanPowerShell: realPowerShell },
}))
Reflect.set(Bun, "which", () => "/bin/true")
Reflect.set(Bun, "spawnSync", () => ({
  exitCode: 0,
  stdout: Buffer.from(""),
  stderr: Buffer.from(""),
  success: true,
}))
