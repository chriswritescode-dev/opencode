use std::collections::BTreeMap;
use std::sync::Arc;

use indexmap::IndexMap;
use opencode_contract::generated::{AbsolutePath, SkillID, SkillInfo, SkillName};
use opencode_registry::events::RegistryEvents;
use opencode_registry::skill::SkillService;
use opencode_tool::builtin::{self, BuiltinDeps};
use opencode_tool::tools::skill::SkillDeps;
use opencode_tool::{SnapshotOptions, ToolCall, ToolContext, ToolRegistry};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::tools::{
    Fixture, PermissionSpec, RecordingAuthorizer, materialize, replace_placeholders, result_value, substitute,
    unique_directory,
};

#[derive(Deserialize)]
struct SkillCase {
    name: String,
    #[serde(default)]
    fixtures: Vec<Fixture>,
    call: SkillCall,
    #[serde(default)]
    permission: PermissionSpec,
    #[serde(default)]
    skills: Vec<SkillFixture>,
    expected: Value,
}

#[derive(Deserialize)]
struct SkillCall {
    id: String,
}

#[derive(Deserialize)]
struct SkillFixture {
    id: String,
    name: String,
    path: String,
    content: String,
    #[serde(default)]
    files: Option<BTreeMap<String, String>>,
}

struct NoEvents;

impl RegistryEvents for NoEvents {
    fn updated(&self, _kind: &'static str) {}
}

#[tokio::test]
async fn skill_corpus_matches() {
    let cases: Vec<SkillCase> =
        serde_json::from_str(include_str!("../corpus/skill.json")).expect("the skill corpus is valid JSON");
    for (index, case) in cases.iter().enumerate() {
        let root = unique_directory(&format!("skill-{index}"));
        let outside = unique_directory(&format!("skill-{index}-outside"));
        materialize(&root, &outside, &case.fixtures);
        let expected = replace_placeholders(case.expected.clone(), &root, &outside);

        let skills = SkillService::new(Arc::new(NoEvents));
        let mut registrations = Vec::new();
        for fixture in &case.skills {
            let info = SkillInfo {
                id: SkillID(fixture.id.clone()),
                name: SkillName(fixture.name.clone()),
                description: Some(fixture.name.clone()),
                autoinvoke: None,
                path: AbsolutePath(substitute(&fixture.path, &root, &outside)),
                content: fixture.content.clone(),
            };
            let files: Option<IndexMap<String, String>> = fixture
                .files
                .as_ref()
                .map(|files| files.iter().map(|(key, value)| (key.clone(), value.clone())).collect());
            registrations.push(skills.transform(move |editor| editor.add(info.clone(), files.as_ref())).await);
        }

        let authorizer = Arc::new(RecordingAuthorizer::new(
            case.permission.allow.unwrap_or(true),
            case.permission.denied_resources.clone(),
        ));
        let deps = BuiltinDeps {
            full_codemode: false,
            skill: Some(SkillDeps { skills: skills.clone() }),
            ..BuiltinDeps::default()
        };
        let registry = ToolRegistry::new();
        registrations.push(builtin::activate(&registry, &deps).await);
        let snapshot = registry.snapshot(&[], &SnapshotOptions::default()).await.expect("the snapshot builds");
        let context = ToolContext {
            session_id: "ses_skill_tool_test".to_owned(),
            agent: "build".to_owned(),
            message_id: "msg_tool_test".to_owned(),
            call_id: format!("call-{index}"),
            authorizer: authorizer.clone(),
            ..ToolContext::default()
        };
        let result = snapshot
            .execute(
                ToolCall {
                    id: format!("call-{index}"),
                    name: "skill".to_owned(),
                    input: json!({ "id": case.call.id }),
                },
                &context,
            )
            .await;
        let actual = result_value(result, &authorizer, &root, &outside);
        assert_eq!(actual, expected, "case {index} ({})", case.name);
        drop(registrations);
        std::fs::remove_dir_all(&root).expect("the test root directory is removed");
        std::fs::remove_dir_all(&outside).expect("the test outside directory is removed");
    }
}
