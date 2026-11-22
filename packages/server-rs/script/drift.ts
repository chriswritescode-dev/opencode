const root = new URL("..", import.meta.url).pathname
const pin = (await Bun.file(`${root}upstream-pin`).text()).trim()
if (!/^[0-9a-f]{40}$/.test(pin)) {
  console.error("drift: upstream-pin must hold one full 40-character commit")
  process.exit(1)
}

const known = Bun.spawnSync(["git", "cat-file", "-e", `${pin}^{commit}`], { cwd: root })
if (known.exitCode !== 0) {
  console.log(`drift: pin ${pin} is not in this clone; ancestry not checked`)
} else {
  const ancestor = Bun.spawnSync(["git", "merge-base", "--is-ancestor", pin, "HEAD"], { cwd: root })
  if (ancestor.exitCode !== 0) {
    console.error(`drift: HEAD does not contain the pinned upstream commit ${pin}`)
    process.exit(1)
  }
}

const before = await snapshot()
const started = performance.now()

for (const script of Array.from(new Bun.Glob("script/gen/*.ts").scanSync({ cwd: root })).sort())
  run(["bun", "run", script], root)

const after = await snapshot()
const changed = Array.from(new Set([...before.keys(), ...after.keys()]))
  .filter((file) => before.get(file) !== after.get(file))
  .sort()
const seconds = ((performance.now() - started) / 1000).toFixed(1)
if (changed.length === 0) {
  console.log(`drift: no generated file changed (${seconds} s)`)
  process.exit(0)
}
console.error(`drift: ${changed.length} file(s) changed (${seconds} s):`)
for (const file of changed) console.error(`  ${file}`)
console.error("drift: commit the regenerated files, or fix the generator if the change was intended.")
process.exit(1)

function run(command: string[], cwd: string) {
  console.log(`drift: ${command.join(" ")}`)
  const proc = Bun.spawnSync(command, { cwd, stdout: "inherit", stderr: "inherit" })
  if (proc.exitCode !== 0) process.exit(proc.exitCode ?? 1)
}

async function snapshot() {
  const top = Bun.spawnSync(["git", "rev-parse", "--show-toplevel"], { cwd: root }).stdout.toString().trim()
  const proc = Bun.spawnSync(["git", "status", "--porcelain", "-z", "--untracked-files=all", "--", "."], { cwd: root })
  const files = proc.stdout
    .toString()
    .split("\0")
    .filter((entry) => entry.length > 3)
    .map((entry) => entry.slice(3))
  return new Map(
    await Promise.all(
      files.map(async (file) => {
        const handle = Bun.file(`${top}/${file}`)
        const hash = (await handle.exists()) ? Bun.hash(await handle.arrayBuffer()).toString() : "deleted"
        return [file, hash] as const
      }),
    ),
  )
}
