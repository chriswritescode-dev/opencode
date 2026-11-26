import { Host } from "@opencode/plugin/host"

export function resolveLocal(input: { directory: string }) {
  return Host.resolve(input)
}
