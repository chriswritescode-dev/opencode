import { createForeignPackageFilter } from "@opencode/plugin/runtime"
import { createLoader } from "@opencode/plugin/runtime-modules"
import { modules } from "./runtime-modules.gen"

let installed: Record<string, () => Record<string, unknown>> | undefined

export function installRuntime() {
  if (installed !== undefined) return
  modules.effect?.()
  const bundled = import.meta.url.endsWith("/plugins.js")
  installed = modules
  Bun.plugin({
    name: "opencode-plugin-runtime",
    setup(build) {
      for (const [specifier, load] of Object.entries(modules)) {
        const loader = bundled ? load : createLoader(Bun.resolveSync(specifier, import.meta.dir))
        build.module(specifier, () => {
          const exports = loader()
          return exports instanceof Promise
            ? exports.then((value) => ({ exports: value, loader: "object" as const }))
            : { exports, loader: "object" as const }
        })
      }
      build.onLoad({ filter: bundled ? createForeignPackageFilter([]) : createForeignPackageFilter() }, (args) => {
        const foreignPkgSuffix = String.raw`(?:node_modules[/\\](@opencode[/\\]plugin|effect)|(@opencode[/\\]plugin|effect)@[^/\\]+@@@\d+)`
        const foreignPkgPattern = new RegExp(String.raw`^(.*[/\\]${foreignPkgSuffix})[/\\](.+)$`)
        const match = args.path.match(foreignPkgPattern)
        const target = match ? `${match[2] ?? match[3]}/${match[4]}`.replaceAll("\\", "/") : args.path
        throw new Error(
          `Cannot load "${target}" from plugin node_modules: "${target}" is not provided by OpenCode; plugins must use the host's "effect" and "@opencode/plugin" modules.`,
        )
      })
    },
  })
}

export function provides(specifier: string) {
  return installed !== undefined && Object.hasOwn(modules, specifier)
}
