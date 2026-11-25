use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::command::{
    BoxFuture, CommandDefinition, CommandEditor, CommandExecute, CommandRuntime, Invocation, Prompt, PromptRequest,
};
use crate::generated;

pub fn command(editor: &mut CommandEditor<'_>, directory: &Path) {
    editor.add(CommandDefinition {
        name: "init".to_string(),
        description: Some("guided AGENTS.md setup".to_string()),
        execute: Arc::new(BuiltinCommand {
            template: generated::INITIALIZE_COMMAND,
            directory: directory.to_path_buf(),
        }),
    });

    editor.add(CommandDefinition {
        name: "review".to_string(),
        description: Some("review changes [commit|branch|pr], defaults to uncommitted".to_string()),
        execute: Arc::new(BuiltinCommand { template: generated::REVIEW_COMMAND, directory: directory.to_path_buf() }),
    });
}

struct BuiltinCommand {
    template: &'static str,
    directory: PathBuf,
}

impl CommandExecute for BuiltinCommand {
    fn execute<'a>(
        &'a self,
        invocation: Invocation,
        runtime: &'a dyn CommandRuntime,
    ) -> BoxFuture<'a, Result<(), String>> {
        let Invocation { session_id, prompt, delivery } = invocation;
        let text = append(&self.template.replacen("${path}", &self.directory.to_string_lossy(), 1), &prompt.text);
        runtime.prompt(PromptRequest { session_id, prompt: Prompt { text, ..prompt }, delivery, resume: None })
    }
}

fn append(template: &str, input: &str) -> String {
    let value = input.trim();
    if template.contains("$ARGUMENTS") {
        return template.replace("$ARGUMENTS", value);
    }
    [template, value].into_iter().filter(|value| !value.is_empty()).collect::<Vec<_>>().join("\n\n")
}
