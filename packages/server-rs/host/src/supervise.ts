export function killAll(pid: number): void {
  const fs = process.getBuiltinModule("node:fs")
  const descendants = () => {
    if (!fs.existsSync("/proc/self/stat")) return []
    const children = new Map<number, number[]>()
    for (const name of fs.readdirSync("/proc")) {
      if (!/^[0-9]+$/.test(name)) continue
      try {
        const stat = fs.readFileSync(`/proc/${name}/stat`, "utf8")
        const ppid = Number(stat.slice(stat.lastIndexOf(")") + 2).split(" ")[1])
        children.set(ppid, [...(children.get(ppid) ?? []), Number(name)])
      } catch {}
    }
    const found: number[] = []
    const queue = [pid]
    while (queue.length) {
      const current = queue.shift()
      if (current === undefined) break
      for (const child of children.get(current) ?? []) {
        found.push(child)
        queue.push(child)
      }
    }
    return found
  }
  for (let round = 0; round < 8; round++) {
    const found = descendants()
    if (!found.length) break
    for (const descendant of found) {
      try {
        process.kill(descendant, "SIGKILL")
      } catch {}
    }
  }
  try {
    process.kill(-pid, "SIGKILL")
  } catch {}
  try {
    process.kill(pid, "SIGKILL")
  } catch {}
}

export function supervise(): void {
  process.on("SIGTERM", () => killAll(process.pid))
  const parent = process.ppid
  const orphan = `const kill = ${killAll.toString()}
const parentOf = () => {
  try {
    const stat = process.getBuiltinModule("node:fs").readFileSync("/proc/self/stat", "utf8")
    return Number(stat.slice(stat.lastIndexOf(")") + 2).split(" ")[1])
  } catch {
    return ${parent}
  }
}
setInterval(() => {
  let gone = parentOf() !== ${parent}
  try {
    process.kill(${parent}, 0)
  } catch {
    gone = true
  }
  if (gone) kill(${process.pid})
}, 100)`
  new Worker(URL.createObjectURL(new Blob([orphan], { type: "application/javascript" }))).unref()
}
