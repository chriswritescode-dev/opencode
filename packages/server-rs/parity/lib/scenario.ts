export type HttpStep = {
  method: string
  path: string
  query?: Record<string, string | string[]>
  headers?: Record<string, string>
  body?: unknown
  auth?: "basic" | "query" | "none"
  location?: boolean
  as?: string
}

export type WaitEventStep = { type: string; match?: Record<string, unknown> }

export type Step =
  | { http: HttpStep }
  | { waitEvent: WaitEventStep; timeout?: number }
  | { files: Record<string, string> }

export type Reply =
  | { text: string }
  | { tool: { id: string; name: string; input: unknown } }
  | { cassette: string; interaction?: number }

export type Scenario = {
  files?: Record<string, string>
  config?: Record<string, unknown>
  env?: Record<string, string>
  model?: Reply[]
  steps: Step[]
}

export function defineScenario(scenario: Scenario): Scenario {
  return scenario
}
