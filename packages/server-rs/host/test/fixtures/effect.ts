import { Effect } from "effect"

export const value = Effect.runSync(Effect.succeed(1))
