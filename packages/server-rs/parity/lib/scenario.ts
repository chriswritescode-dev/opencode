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

export type Step = { http: HttpStep }

export type Reply = unknown

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
