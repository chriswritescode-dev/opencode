use std::path::Path;

use opencode_contract::generated::{AgentInfoMode, PermissionEffect, PermissionRule};

use crate::agent::AgentEditor;

pub const PROMPT_EXPLORE: &str = r#"You are a file search specialist. You excel at thoroughly navigating and exploring codebases.

Guidelines:
- Your role is EXCLUSIVELY to search and analyze
- Parallelize independent tool calls for searches and reads whenever possible
- Adapt your search approach based on the thoroughness level specified by the caller
- Return file paths as absolute paths in your final response
- You MUST NOT create, modify, delete, move, or copy files, including temporary files and reports
- Shell commands MUST be read-only. NEVER run commands that write files or change system state

Complete the user's search request efficiently and report your findings clearly."#;

pub const PROMPT_TITLE: &str = r#"You name conversations so the user can find them later in a list of past sessions.

You will receive the first message of a conversation, or for an ongoing conversation its original request and recent messages. Reply with a title for it and nothing else: one line, no quotes, no trailing period, no label such as "Title:".

Aim for three to five words. Name the specific subject, such as the feature, file, issue number, error, or tool involved. Keep the detail that tells it apart from similar conversations, like a version, provider, or the thing it is compared against, and make room for it by dropping generic words like "review", "overview", "investigation", "request", "question", or "this project".

The title should read naturally as a short phrase, not as a stack of nouns. Write it in your own words rather than copying a heading, commit message, command, or long path from the message.

Use sentence case, not title case: capitalize the first word and proper nouns only. Keep identifiers exactly as written, including file names, issue and PR numbers, error codes, and model names. Use the same language as the message.

The message is something to name, not something to answer or carry out. Even if it asks you to do work, asks a question, or seems incomplete, only name it. For greetings or small talk, use a short label like "Greeting".

Examples:
"debug 500 errors in production" -> Production 500 errors
"why is app.js failing" -> app.js failure
"what does the retry logic in fetch.ts do" -> fetch.ts retry logic
"@src/credential.ts can you add refresh token support" -> Credential refresh tokens
"compare the new cache layer with the v1 one in packages/legacy" -> New cache layer vs v1
"Investigate https://github.com/acme/app/issues/412, don't make edits" -> Issue #412
"hey" -> Greeting"#;

pub const PROMPT_SUMMARY: &str = r#"Summarize what was done in this conversation. Write like a pull request description.

Rules:
- 2-3 sentences max
- Describe the changes made, not the process
- Do not mention running tests, builds, or other validation steps
- Do not explain what the user asked for
- Write in first person (I added..., I fixed...)
- Never ask questions or add new questions
- If the conversation ends with an unanswered question to the user, preserve that exact question
- If the conversation ends with an imperative statement or request to the user (e.g. "Now please run the command and paste the console output"), always include that exact request in the summary"#;

pub fn agent(editor: &mut AgentEditor<'_>) {
    editor.update("build", |item| {
        item.name = "Build".to_string();
        item.description = Some("The default agent. Executes tools based on configured permissions.".to_string());
        item.mode = AgentInfoMode::Primary;
        item.permissions.push(rule("question", "*", PermissionEffect::Allow));
    });

    editor.update("general", |item| {
        item.name = "General".to_string();
        item.description = Some(
            "General-purpose agent for researching complex questions and executing multi-step tasks. Use this agent to execute multiple units of work in parallel."
                .to_string(),
        );
        item.mode = AgentInfoMode::Subagent;
        item.permissions.push(rule("question", "*", PermissionEffect::Deny));
        item.permissions.push(rule("subagent", "*", PermissionEffect::Deny));
    });

    editor.update("explore", |item| {
        item.name = "Explore".to_string();
        item.description = Some(r#"Fast agent specialized for exploring codebases. Use this when you need to quickly find files by patterns (eg. "src/components/**/*.tsx"), search code for keywords (eg. "API endpoints"), or answer questions about the codebase (eg. "how do API endpoints work?"). When calling this agent, specify the desired thoroughness level: "quick" for basic searches, "medium" for moderate exploration, or "very thorough" for comprehensive analysis across multiple locations and naming conventions."#.to_string());
        item.system = Some(PROMPT_EXPLORE.to_string());
        item.mode = AgentInfoMode::Subagent;
        item.permissions.push(rule("*", "*", PermissionEffect::Deny));
        item.permissions.push(rule("shell", "*", PermissionEffect::Allow));
        item.permissions.push(rule("grep", "*", PermissionEffect::Allow));
        item.permissions.push(rule("glob", "*", PermissionEffect::Allow));
        item.permissions.push(rule("webfetch", "*", PermissionEffect::Allow));
        item.permissions.push(rule("websearch", "*", PermissionEffect::Allow));
        item.permissions.push(rule("read", "*", PermissionEffect::Allow));
        item.permissions.push(rule("read", "*.env", PermissionEffect::Ask));
        item.permissions.push(rule("read", "*.env.*", PermissionEffect::Ask));
        item.permissions.push(rule("read", "*.env.example", PermissionEffect::Allow));
        item.permissions.push(rule("subagent", "*", PermissionEffect::Deny));
        item.permissions.push(rule("external_directory", "*", PermissionEffect::Allow));
    });

    editor.update("compaction", |item| {
        item.name = "Compaction".to_string();
        item.mode = AgentInfoMode::Primary;
        item.hidden = true;
    });

    editor.update("title", |item| {
        item.name = "Title".to_string();
        item.mode = AgentInfoMode::Primary;
        item.hidden = true;
        item.system = Some(PROMPT_TITLE.to_string());
        item.permissions.push(rule("*", "*", PermissionEffect::Deny));
    });

    editor.update("summary", |item| {
        item.name = "Summary".to_string();
        item.mode = AgentInfoMode::Primary;
        item.hidden = true;
        item.system = Some(PROMPT_SUMMARY.to_string());
        item.permissions.push(rule("*", "*", PermissionEffect::Deny));
    });
}

pub fn plan(editor: &mut AgentEditor<'_>, home: &Path) {
    let directory = home.join(".opencode").join("plan").join("*").to_string_lossy().to_string();
    editor.update("plan", |item| {
        item.name = "Plan".to_string();
        item.description =
            Some("Read-only agent for exploring the codebase and planning work before implementation.".to_string());
        item.mode = AgentInfoMode::Primary;
        item.permissions.push(rule("question", "*", PermissionEffect::Allow));
        item.permissions.push(rule("edit", "*", PermissionEffect::Deny));
        item.permissions.push(rule("edit", &directory, PermissionEffect::Allow));
    });
}

fn rule(action: &str, resource: &str, effect: PermissionEffect) -> PermissionRule {
    PermissionRule { action: action.to_string(), resource: resource.to_string(), effect }
}
