import { mkdir, realpath, rm, symlink } from "node:fs/promises"
import { dirname } from "node:path"

const root = new URL("..", import.meta.url).pathname
const repository = gitTopLevel()

export async function pin() {
  const value = (await Bun.file(`${root}upstream-pin`).text()).trim()
  if (!/^[0-9a-f]{40}$/.test(value)) {
    console.error("upstream: upstream-pin must hold one full 40-character commit")
    process.exit(1)
  }
  return value
}

export async function upstreamTree(commit?: string) {
  const revision = commit ?? (await pin())
  const directory = `${root}target/upstream/${revision}`
  if (await Bun.file(`${directory}/.complete`).exists()) return directory
  await rm(directory, { recursive: true, force: true })
  await mkdir(directory, { recursive: true })
  await extract(revision, directory)
  await requireVersions(revision)
  await link(directory)
  await Bun.write(`${directory}/.complete`, "")
  return directory
}

function gitTopLevel() {
  const result = Bun.spawnSync(["git", "rev-parse", "--show-toplevel"], { cwd: root })
  if (result.exitCode !== 0) {
    console.error(`upstream: git failed: ${result.stderr.toString().trim()}`)
    process.exit(1)
  }
  return `${result.stdout.toString().trim()}/`
}

async function extract(commit: string, directory: string) {
  const archive = Bun.spawn(
    ["git", "archive", commit, "packages/protocol", "packages/schema", "packages/httpapi-codegen"],
    { cwd: repository, stdout: "pipe", stderr: "inherit" },
  )
  const tar = Bun.spawn(["tar", "-x", "-C", directory], { stdin: archive.stdout, stdout: "inherit", stderr: "inherit" })
  const [archiveCode, tarCode] = await Promise.all([archive.exited, tar.exited])
  if (archiveCode !== 0 || tarCode !== 0) {
    console.error(`upstream: materializing ${commit} failed`)
    process.exit(1)
  }
}

async function requireVersions(commit: string) {
  const result = Bun.spawnSync(["git", "show", `${commit}:package.json`], { cwd: repository })
  if (result.exitCode !== 0) {
    console.error(`upstream: git show ${commit}:package.json failed`)
    process.exit(1)
  }
  const manifest = JSON.parse(result.stdout.toString())
  await requireVersion(commit, "effect", manifest.workspaces?.catalog?.effect)
  await requireVersion(commit, "prettier", manifest.workspaces?.catalog?.prettier ?? manifest.devDependencies?.prettier)
}

async function requireVersion(commit: string, name: string, expected: string) {
  const installed = (await installedVersion(name)) ?? "nothing"
  if (installed === expected) return
  console.error(
    `upstream: ${commit} needs ${name} ${expected}, installed ${installed}; re-sync the pin or install the matching version`,
  )
  process.exit(1)
}

async function installedVersion(name: string) {
  const path = installedPackage(name)
  return path === undefined ? undefined : (await Bun.file(`${path}/package.json`).json()).version
}

function installedPackage(name: string, from = root) {
  try {
    return dirname(Bun.resolveSync(`${name}/package.json`, from))
  } catch {
    return undefined
  }
}

async function link(directory: string) {
  const modules = `${directory}/node_modules`
  await mkdir(`${modules}/@opencode`, { recursive: true })
  await mkdir(`${modules}/@standard-schema`, { recursive: true })
  for (const name of ["schema", "protocol", "httpapi-codegen"])
    await symlink(`../../packages/${name}`, `${modules}/@opencode/${name}`)
  await symlink(await realpath(requiredPackage("effect")), `${modules}/effect`)
  await symlink(await realpath(requiredPackage("prettier")), `${modules}/prettier`)
  await symlink(
    await realpath(requiredPackage("@standard-schema/spec", `${repository}packages/schema`)),
    `${modules}/@standard-schema/spec`,
  )
}

function requiredPackage(name: string, from = root) {
  const path = installedPackage(name, from)
  if (path === undefined) {
    console.error(`upstream: ${name} is not installed; run bun install`)
    process.exit(1)
  }
  return path
}
