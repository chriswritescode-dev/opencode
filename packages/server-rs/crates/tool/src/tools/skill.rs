use futures_util::future::BoxFuture;
use opencode_registry::skill::{self, SkillService};
use serde_json::{Value, json};

use crate::context::{Content, ToolContext, ToolError, ToolOutput, ToolRuntime, permission_input};

#[derive(Clone)]
pub struct SkillDeps {
    pub skills: SkillService,
}

pub struct SkillRuntime {
    deps: SkillDeps,
}

impl SkillRuntime {
    pub fn new(deps: SkillDeps) -> Self {
        SkillRuntime { deps }
    }

    async fn execute_inner(&self, input: Value, context: &ToolContext) -> Result<ToolOutput, ToolError> {
        let id = input.get("id").and_then(Value::as_str).unwrap_or_default().to_owned();
        let found = self.deps.skills.get(&id).await.map_err(|_| unable(&id))?;
        let Some(skill) = found else { return Err(unable(&id)) };
        context
            .authorizer
            .assert(permission_input(context, "skill", vec![skill.id.0.clone()], vec![skill.id.0.clone()]))
            .await
            .map_err(|error| ToolError::new(error.to_string()))?;
        let files = self.deps.skills.files(&skill.id.0).await.map_err(|_| unable(&id))?;
        let prepared = skill::prepare(&skill, files.as_ref());
        let directory = prepared.directory;
        let content = prepared.output;
        Ok(ToolOutput {
            output: json!({ "directory": directory.clone(), "content": content.clone() }),
            content: vec![Content::text(content)],
            metadata: Some(json!({ "name": skill.name.0, "directory": directory })),
        })
    }
}

impl ToolRuntime for SkillRuntime {
    fn execute<'a>(&'a self, input: Value, context: &'a ToolContext) -> BoxFuture<'a, Result<ToolOutput, ToolError>> {
        Box::pin(self.execute_inner(input, context))
    }
}

fn unable(id: &str) -> ToolError {
    ToolError::new(format!("Unable to load skill {id}"))
}
