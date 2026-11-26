import { chmod, mkdir, rename, rm, stat } from "node:fs/promises"
import path from "node:path"

const version = "0.6.0"
const destinationRoot = new URL(`../target/opencode-pty/${version}`, import.meta.url).pathname
const executable = process.platform === "win32" ? "opencode-pty.exe" : "opencode-pty"
const destination = path.join(destinationRoot, executable)

try {
  if (await exists(destination)) {
    console.log(destination)
    process.exit(0)
  }

  const name = packageName()
  const url = `https://registry.npmjs.org/@opencode/${name}/-/${name}-${version}.tgz`
  const response = await fetch(url)
  if (!response.ok) throw new Error(`${url} responded ${response.status}`)

  const workspace = path.join(destinationRoot, `.download-${process.pid}-${crypto.randomUUID()}`)
  const archive = path.join(workspace, "package.tgz")
  const extract = path.join(workspace, "extract")
  await mkdir(extract, { recursive: true })
  try {
    await Bun.write(archive, new Uint8Array(await response.arrayBuffer()))
    const result = Bun.spawnSync(["tar", "-xzf", archive, "-C", extract], { stdout: "pipe", stderr: "pipe" })
    if (result.exitCode !== 0)
      throw new Error(`tar failed: ${result.stderr.toString().trim() || result.exitCode}`)
    const source = path.join(extract, "package", "bin", executable)
    if (!(await exists(source))) throw new Error(`${name} has no ${executable}`)
    await mkdir(destinationRoot, { recursive: true, mode: 0o700 })
    await chmod(source, 0o755)
    await rename(source, destination)
  } finally {
    await rm(workspace, { recursive: true, force: true })
  }

  console.log(destination)
} catch (error) {
  console.error(`pty-binary: ${error instanceof Error ? error.message : String(error)}`)
  process.exit(1)
}

function packageName() {
  const arch = process.arch
  if (arch !== "x64" && arch !== "arm64") throw new Error(`unsupported architecture ${arch}`)
  if (process.platform === "linux") return `pty-linux-${arch}-${libc()}`
  if (process.platform === "darwin") return `pty-darwin-${arch}`
  if (process.platform === "win32") return `pty-win32-${arch}`
  throw new Error(`unsupported platform ${process.platform}`)
}

function libc() {
  const result = Bun.spawnSync(["ldd", "--version"], { stdout: "pipe", stderr: "pipe" })
  const output = `${result.stdout.toString()} ${result.stderr.toString()}`.toLowerCase()
  return output.includes("musl") ? "musl" : "gnu"
}

async function exists(file: string) {
  try {
    return (await stat(file)).isFile()
  } catch {
    return false
  }
}
