import { mkdir, mkdtemp, rm, writeFile } from "node:fs/promises"
import os from "node:os"
import path from "node:path"

type Files = Record<string, string>
type Packages = Record<string, Record<string, Files>>

type Publication = { version: string; tarballPath: string; integrity: string }

export async function startRegistry(packages: Packages, options: { delayMs?: number } = {}) {
  const tarballs = new Map<string, Uint8Array>()
  const publications = new Map<string, Publication[]>()
  const packumentPaths = new Map<string, string>()

  async function pack(name: string, version: string, files: Files) {
    const tmp = await mkdtemp(path.join(os.tmpdir(), "registry-"))
    try {
      const entry = path.join(tmp, "package")
      await mkdir(entry, { recursive: true })
      if (!("package.json" in files)) {
        await writeFile(
          path.join(entry, "package.json"),
          JSON.stringify({ name, version, exports: "./index.js" }),
        )
      }
      for (const [file, content] of Object.entries(files)) {
        const target = path.join(entry, file)
        await mkdir(path.dirname(target), { recursive: true })
        await writeFile(target, content)
      }
      const tarball = path.join(tmp, "package.tgz")
      const result = Bun.spawnSync(["tar", "-czf", tarball, "-C", tmp, "package"])
      if (result.exitCode !== 0) throw new Error(result.stderr.toString())
      return await Bun.file(tarball).bytes()
    } finally {
      await rm(tmp, { recursive: true, force: true })
    }
  }

  async function publish(name: string, version: string, files: Files) {
    const bytes = await pack(name, version, files)
    const basename = name.slice(name.lastIndexOf("/") + 1)
    const tarballPath = `/${name}/-/${basename}-${version}.tgz`
    const integrity = `sha512-${new Bun.CryptoHasher("sha512").update(bytes).digest("base64")}`
    tarballs.set(tarballPath, bytes)
    const versions = publications.get(name) ?? []
    versions.push({ version, tarballPath, integrity })
    publications.set(name, versions)
    packumentPaths.set(`/${name}`, name)
  }

  for (const [name, versions] of Object.entries(packages)) {
    for (const [version, files] of Object.entries(versions)) await publish(name, version, files)
  }

  const server = Bun.serve({
    hostname: "127.0.0.1",
    port: 0,
    async fetch(request) {
      const url = new URL(request.url)
      const name = packumentPaths.get(url.pathname)
      if (name) {
        const published = publications.get(name) ?? []
        return Response.json({
          name,
          "dist-tags": { latest: published.at(-1)?.version },
          versions: Object.fromEntries(
            published.map((entry) => [
              entry.version,
              {
                name,
                version: entry.version,
                dist: { tarball: `${url.origin}${entry.tarballPath}`, integrity: entry.integrity },
              },
            ]),
          ),
        })
      }
      const tarball = tarballs.get(url.pathname)
      if (!tarball) return new Response("Not Found", { status: 404 })
      if (options.delayMs) await Bun.sleep(options.delayMs)
      return new Response(tarball, { headers: { "content-type": "application/octet-stream" } })
    },
  })

  return {
    url: `http://127.0.0.1:${server.port}`,
    publish,
    async stop() {
      await server.stop(true)
    },
  }
}
