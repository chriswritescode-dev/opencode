# Deviations

A difference from the TypeScript server at the commit in `../../upstream-pin` is allowed only as a file in this
directory that the owner has accepted. A difference without a file is a failure.

## Format

Files are named `<area>-<slug>.md`, where the area is the task that owns the difference (`OR-52`) or the port area
(`session`, `pty`) and the slug is short and kebab-case. The first line is the heading
`# <area>: <one-line difference>`.

Each file has these paragraphs, in order:

- `TypeScript:` what the TypeScript server does, with `path:line` at the pinned commit.
- `Rust:` what this package does instead, with the crate and function or file.
- `Reason:` why the difference exists, and which parity scenario or check covers it.
- `Accepted:` the owner's name and the date the deviation was accepted.

## Current deviations

- `OR-38-lone-surrogates.md`: the strict round-trip corpus skips samples with unpaired UTF-16 surrogates
- `OR-38-request-bodies.md`: request bodies decode into the wire representation
- `OR-38-request-non-finite-numbers.md`: non-finite numbers in string slots are rejected
