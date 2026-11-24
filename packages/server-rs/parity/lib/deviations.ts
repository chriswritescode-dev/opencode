import fs from "node:fs/promises"
import path from "node:path"
import type { Difference } from "./compare"

export type DeviationRule = { scenario: string; path: string }
export type Deviation = { id: string; accepted: boolean; rules: DeviationRule[] | undefined }
export type Explained = { difference: Difference; id: string }

export async function loadDeviations(directory: string): Promise<Deviation[]> {
  const names = (await fs.readdir(directory))
    .filter((name) => name.endsWith(".md") && name !== "README.md")
    .sort()
  return Promise.all(
    names.map(async (name) => {
      const id = name.replace(/\.md$/, "")
      const text = await Bun.file(path.join(directory, name)).text()
      return { id, accepted: isAccepted(text), rules: parseRules(id, text) }
    }),
  )
}

function isAccepted(text: string) {
  return text.split(/\r?\n/).some((line) => {
    const trimmed = line.trim()
    if (!trimmed.startsWith("Accepted:")) return false
    const value = trimmed.slice("Accepted:".length).trim()
    return value.length > 0 && !value.toLowerCase().startsWith("pending")
  })
}

function parseRules(id: string, text: string): DeviationRule[] | undefined {
  const rules: DeviationRule[] = []
  let block: string[] | undefined
  let found = false
  for (const line of text.split(/\r?\n/)) {
    const trimmed = line.trim()
    if (block === undefined) {
      if (trimmed !== "```parity") continue
      block = []
      found = true
      continue
    }
    if (trimmed !== "```") {
      if (trimmed.length > 0) block.push(trimmed)
      continue
    }
    for (const rule of block) {
      const tokens = rule.split(/\s+/)
      if (tokens.length !== 2) throw new Error(`deviation ${id} has an invalid parity rule: ${rule}`)
      rules.push({ scenario: tokens[0], path: tokens[1] })
    }
    block = undefined
  }
  if (block !== undefined) throw new Error(`deviation ${id} has an unclosed parity block`)
  return found ? rules : undefined
}

export function explain(differences: Difference[], scenario: string, deviations: Deviation[]) {
  const rules = deviations.flatMap((deviation) =>
    deviation.accepted && deviation.rules !== undefined
      ? deviation.rules.map((rule) => ({
          id: deviation.id,
          scenario: new Bun.Glob(rule.scenario),
          path: new Bun.Glob(rule.path),
        }))
      : [],
  )
  const explained: Explained[] = []
  const unexplained: Difference[] = []
  for (const difference of differences) {
    const match = rules.find((rule) => rule.scenario.match(scenario) && rule.path.match(difference.path))
    if (match === undefined) unexplained.push(difference)
    else explained.push({ difference, id: match.id })
  }
  return { explained, unexplained }
}

export function deviationWarnings(deviations: Deviation[], matched: Set<string>) {
  return deviations.flatMap((deviation) => {
    if (!deviation.accepted) return [`warning: deviation ${deviation.id} is not accepted`]
    if (matched.has(deviation.id)) return []
    return [`warning: deviation ${deviation.id} matched nothing`]
  })
}
