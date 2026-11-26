# OR-48: the default Console organization is ordered without locale collation

TypeScript: without a device-token organization, the sign-in picks the first organization by
`a.name.localeCompare(b.name) || a.id.localeCompare(b.id)` (`packages/core/src/plugin/provider/opencode.ts:836`), so the
order follows the runtime's ICU collation: case, accents and punctuation follow the collation rules.

Rust: `locale_order` (`crates/catalog/src/provider_plugin/opencode/sign_in.rs:128-130`) compares the lowercased names
bytewise and breaks an equal fold with the reverse byte order, so the earlier of two names that differ only in case sorts
lowercase first. That matches the common Latin case-insensitive order, but an accent, a combining mark or punctuation
inside a name can order differently from `localeCompare`.

Reason: the Rust server has no ICU collation dependency for this one comparison. The ported tests
`uses_the_first_organization_by_name_without_a_device_token_organization` and
`picks_the_first_organization_by_locale_name_without_a_device_token_organization` cover the ASCII and case-tie path;
no scenario covers accented organization names.

Accepted: pending owner
