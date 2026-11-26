# OR-49: the model request omits the skill tool definition TypeScript keeps when every tool is denied

TypeScript filters a snapshot tool when the last permission rule matching its action has resource `*` and effect
`deny` (`whollyDisabled` in `packages/core/src/tool.ts`). The browser plugin appends
`{ action: "skill", resource: "browser", effect: "deny" }` after the configured rules, so with `prompt-text-only`'s
deny-all the last `skill` rule's resource is `browser`. The whole-tool check is therefore false and the skill
definition stays in the request. Skill guidance still disappears, because `Permission.evaluate("skill", <id>, rules)`
matches both rules on the skill's own id and lands on the deny-all.

Rust (`crates/tool/src/snapshot.rs`) has no browser plugin rule, so the same deny-all filters the skill tool and the
request carries no tools.

Reason: the browser plugin and the permission rules it contributes are not ported yet.

Accepted: pending owner

```parity
prompt-text-only model/*/body/tools
prompt-text-only model/*/body/tools/**
```
