# OR-51: the output store reports Rust IO errors and validates inputs from JSON Schema

TypeScript: `packages/core/src/tool-output.ts:52-56,98-115` describes a failed save with the Node system error message (`ENOTDIR: not a directory, mkdir '/…/tool-output'`). `packages/core/src/tool/runtime.ts:76-113` validates a call against the tool's Effect or Standard Schema, so annotations reach the message: `grep.pattern` reports `Pattern must not be empty`, and the branded `RelativePath` and `Skill.ID` strings report their identifiers.

Rust: `crates/tool/src/output.rs` builds the marker from `std::io::Error`'s display text (`Not a directory (os error 20)`), so only the detail after `full output could not be saved: ` differs; the limits, the preview, the marker prefix and suffix, and `metadata.truncated`/`metadata.outputPath` match. `crates/tool/src/input.rs` validates the generated JSON Schema, which cannot carry Effect annotations, so the messages above are supplied by explicit table entries for the ported tools (`grep.pattern`, `glob.path`, `grep.path`, `skill.id`). A tool with another custom annotation, such as a plugin tool from OR-62, would report the JSON Schema default. The runtime also passes `output` through instead of re-encoding it against the output schema, because no ported tool has a transforming or branded output codec yet.

Reason: the Rust server has no Node errno formatting, and JSON Schema does not retain the Effect annotations the generated definitions were built from. `crates/tool/tests/corpus/validation.json` covers invalid inputs for every ported tool and `truncate.json` covers the `tool-output.test.ts` cases; the save-failure case compares the marker around a normalized error placeholder, and the cleanup test covers the file-retention case.

Accepted: pending owner

```parity
```
