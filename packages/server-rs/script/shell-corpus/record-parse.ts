import * as bunTest from "bun:test"
import fs from "node:fs"
import os from "node:os"
import { appendFileSync } from "node:fs"
import { Effect } from "effect"
import * as real from "../src/shell/parse.ts"

const realScan = real.scan

const output = process.env.OPENCODE_PARSE_CORPUS_OUT ?? ""
const home = process.env.OPENCODE_PARSE_HOME ?? ""
const user = process.env.OPENCODE_PARSE_USER ?? ""
const passwd = process.env.OPENCODE_PARSE_PASSWD ?? ""

const passwdTable = Object.fromEntries(
  passwd
    .split("\n")
    .filter(Boolean)
    .flatMap((line) => {
      const fields = line.split(":")
      return fields[0] && fields[5] ? [[fields[0], fields[5]]] : []
    }),
)

const originalRead = fs.readFileSync
Reflect.set(fs, "readFileSync", (path: string, options?: unknown) =>
  path === "/etc/passwd" ? passwd : originalRead(path, options as never),
)
Reflect.set(os, "homedir", () => home)
Reflect.set(os, "userInfo", () => ({ username: user, homedir: home, uid: 0, gid: 0, shell: null }))
process.env.HOME = home

const source = () => (new Error().stack ?? "").match(/test\/shell-parse[^/\n]*\.test\.ts:\d+/)?.[0] ?? "unknown"

const environment = (command: string) => {
  const keys = new Set(["HOME", "USERPROFILE"])
  for (const match of command.matchAll(/\$\{env:([^}]+)\}/gi)) keys.add(match[1] ?? "")
  for (const match of command.matchAll(/\$env:([A-Za-z_][A-Za-z0-9_]*)/gi)) keys.add(match[1] ?? "")
  return Object.fromEntries([...keys].flatMap((key) => (process.env[key] === undefined ? [] : [[key, process.env[key]]])))
}

const scan = (command: string, shell: string, cwd: string, options: { home?: string | null } = {}) => {
  const result = Effect.runSync(Effect.result(realScan(command, shell, cwd, options)))
  const record = {
    command,
    shell,
    cwd,
    tildeHome: options.home === null ? null : (options.home ?? home),
    systemHome: home,
    environment: environment(command),
    passwd: passwdTable,
    currentUser: { name: user, home },
    source: source(),
  }
  if (result._tag === "Success") appendFileSync(output, `${JSON.stringify({ ...record, result: result.success })}\n`)
  else appendFileSync(output, `${JSON.stringify({ ...record, error: result.failure.reason })}\n`)
  return result._tag === "Success" ? Effect.succeed(result.success) : Effect.fail(result.failure)
}

bunTest.mock.module("../src/shell/parse.ts", () => ({ ShellParse: { scan }, scan }))
