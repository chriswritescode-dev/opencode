import { mkdir, realpath, rm, symlink } from "node:fs/promises"
import { dirname } from "node:path"

const root = new URL("..", import.meta.url).pathname
const repository = gitTopLevel()

/** Packages extracted from the pin. The marker records this list so an older tree is replaced. */
const paths = [
  "packages/ai",
  "packages/http-recorder",
  "packages/httpapi-codegen",
  "packages/protocol",
  "packages/schema",
]
const marker = paths.join("\n")

/** Packages linked for their own sake rather than through a pinned manifest. */
const extra = ["effect", "prettier"]

/** Tooling is used by the repository, not by the exporter, and its catalog entries use npm aliases. */
const tooling = new Set(["typescript", "@typescript/native", "@clack/prompts"])

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
  await requireVersions(revision)
  const directory = `${root}target/upstream/${revision}`
  if (
    (await Bun.file(`${directory}/.complete`)
      .text()
      .catch(() => "")) === marker
  )
    return directory
  await rm(directory, { recursive: true, force: true })
  await mkdir(directory, { recursive: true })
  await extract(revision, directory)
  await link(directory)
  await Bun.write(`${directory}/.complete`, marker)
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
  const archive = Bun.spawn(["git", "archive", commit, ...paths], {
    cwd: repository,
    stdout: "pipe",
    stderr: "inherit",
  })
  const tar = Bun.spawn(["tar", "-x", "-C", directory], { stdin: archive.stdout, stdout: "inherit", stderr: "inherit" })
  const [archiveCode, tarCode] = await Promise.all([archive.exited, tar.exited])
  if (archiveCode !== 0 || tarCode !== 0) {
    console.error(`upstream: materializing ${commit} failed`)
    process.exit(1)
  }
}

async function requireVersions(commit: string) {
  const manifest = gitJson(commit, "package.json")
  const catalog: Record<string, string> = manifest.workspaces?.catalog ?? {}
  await requireVersion(commit, "effect", catalog.effect)
  await requireVersion(commit, "prettier", catalog.prettier ?? manifest.devDependencies?.prettier)
  for (const path of ["packages/ai", "packages/http-recorder"]) {
    const packages = gitJson(commit, `${path}/package.json`)
    for (const name of linkedNames(packages)) {
      const expected = declaredVersion(packages, catalog, name)
      if (expected === undefined) {
        console.error(`upstream: ${commit} has no version for ${name}; re-sync the pin or the catalog`)
        process.exit(1)
      }
      await requireVersion(commit, name, expected, `${repository}${path}`)
    }
  }
}

function gitJson(commit: string, path: string) {
  const result = Bun.spawnSync(["git", "show", `${commit}:${path}`], { cwd: repository })
  if (result.exitCode !== 0) {
    console.error(`upstream: git show ${commit}:${path} failed`)
    process.exit(1)
  }
  return JSON.parse(result.stdout.toString())
}

/** Runtime and tooling packages the exporter resolves through the extracted tree. */
function linkedNames(packages: {
  readonly dependencies?: Record<string, string>
  readonly devDependencies?: Record<string, string>
}) {
  return [...Object.keys(packages.dependencies ?? {}), ...Object.keys(packages.devDependencies ?? {})]
    .filter((name) => !name.startsWith("@opencode/"))
    .filter((name) => !tooling.has(name))
}

function declaredVersion(
  packages: {
    readonly dependencies?: Record<string, string>
    readonly devDependencies?: Record<string, string>
  },
  catalog: Record<string, string>,
  name: string,
) {
  const spec = packages.dependencies?.[name] ?? packages.devDependencies?.[name]
  if (spec === undefined) return undefined
  if (spec === "catalog:") return catalog[name]
  return spec
}

async function requireVersion(commit: string, name: string, expected: string, from = root) {
  const installed = (await installedVersion(name, from)) ?? "nothing"
  if (installed === expected) return
  console.error(
    `upstream: ${commit} needs ${name} ${expected}, installed ${installed}; re-sync the pin or install the matching version`,
  )
  process.exit(1)
}

async function installedVersion(name: string, from = root) {
  const path = installedPackage(name, from)
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
  for (const name of ["schema", "protocol", "httpapi-codegen", "ai", "http-recorder"])
    await symlink(`../../packages/${name}`, `${modules}/@opencode/${name}`)
  for (const name of extra) await linkPackage(modules, name)
  await linkPackage(modules, "@standard-schema/spec", `${repository}packages/schema`)
  for (const path of ["packages/ai", "packages/http-recorder"])
    for (const name of linkedNames(await Bun.file(`${directory}/${path}/package.json`).json()))
      await linkPackage(modules, name, `${repository}${path}`)
}

async function linkPackage(modules: string, name: string, from = root) {
  const path = `${modules}/${name}`
  if (await Bun.file(`${path}/package.json`).exists()) return
  await mkdir(dirname(path), { recursive: true })
  await symlink(await realpath(requiredPackage(name, from)), path)
}

function requiredPackage(name: string, from = root) {
  const path = installedPackage(name, from)
  if (path === undefined) {
    console.error(`upstream: ${name} is not installed; run bun install`)
    process.exit(1)
  }
  return path
}
