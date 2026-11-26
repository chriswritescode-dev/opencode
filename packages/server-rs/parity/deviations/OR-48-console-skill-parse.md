# OR-48: Console skills keep their `SKILL.md` unparsed

TypeScript: `packages/core/src/plugin/provider/opencode.ts:776` parses the downloaded `SKILL.md` with
`SkillFile.parse(`/${slug}`, `/${slug}/SKILL.md`, main.content)` and `:777` fails the download with
`skill ${slug} has an invalid SKILL.md` when the frontmatter or body does not parse. `downloadSkill` returns the
parsed `Skill.Info` and the console plugin registers it through `skills.transform` at `:517`.

Rust: `crates/catalog/src/provider_plugin/opencode/content.rs` `download_skill` returns
`ConsoleSkill { slug, main, files }`, where `main` is the raw `SKILL.md` content and `files` holds every other
downloaded file. `ConsoleHandle::skills()` exposes those values, and the plugin records them for the skill
registry, but nothing parses `SKILL.md` yet, so a malformed skill stays visible instead of failing its download.

Reason: the Rust skill registry is OR-50. Its `skills.transform` parses `main` when it registers Console skills, so
the parse and the `skill ${slug} has an invalid SKILL.md` failure move there. This task keeps the download and the
ownership rules only.

Accepted: pending owner
