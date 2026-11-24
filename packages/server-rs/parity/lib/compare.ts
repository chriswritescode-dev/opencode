export type Difference = { path: string; reference: unknown; candidate: unknown }

const join = (path: string, key: string) => (path.length === 0 ? key : `${path}/${key}`)

const isObject = (value: unknown): value is Record<string, unknown> =>
  typeof value === "object" && value !== null && !Array.isArray(value)

const walk = (path: string, reference: unknown, candidate: unknown, differences: Difference[]) => {
  if (Array.isArray(reference) && Array.isArray(candidate)) {
    const length = Math.max(reference.length, candidate.length)
    for (let index = 0; index < length; index += 1) {
      const elementPath = join(path, String(index))
      if (index >= reference.length) differences.push({ path: elementPath, reference: undefined, candidate: candidate[index] })
      else if (index >= candidate.length)
        differences.push({ path: elementPath, reference: reference[index], candidate: undefined })
      else walk(elementPath, reference[index], candidate[index], differences)
    }
    return
  }
  if (isObject(reference) && isObject(candidate)) {
    for (const key of Object.keys(reference)) {
      const keyPath = join(path, key)
      if (Object.hasOwn(candidate, key)) walk(keyPath, reference[key], candidate[key], differences)
      else differences.push({ path: keyPath, reference: reference[key], candidate: undefined })
    }
    for (const key of Object.keys(candidate)) {
      if (Object.hasOwn(reference, key)) continue
      differences.push({ path: join(path, key), reference: undefined, candidate: candidate[key] })
    }
    return
  }
  if (reference !== candidate) differences.push({ path, reference, candidate })
}

export function compareCaptures(reference: unknown, candidate: unknown): Difference[] {
  const differences: Difference[] = []
  walk("", reference, candidate, differences)
  return differences
}
