use crate::config;

pub mod agent;
pub mod command;
pub mod skill;

pub struct Deps {
    pub config: opencode_config::service::ConfigService,
    pub agents: crate::agent::AgentService,
    pub commands: crate::command::CommandService,
    pub skills: crate::skill::SkillService,
    pub references: crate::reference::ReferenceService,
    pub instructions: crate::instructions::discovery::InstructionDiscovery,
    pub directory: std::path::PathBuf,
    pub project_directory: std::path::PathBuf,
    pub home: std::path::PathBuf,
    pub config_directory: std::path::PathBuf,
    pub cache: std::path::PathBuf,
    pub app: skill::AppInfo,
    #[allow(clippy::type_complexity)]
    pub env: std::sync::Arc<dyn Fn(&str) -> Option<String> + Send + Sync>,
}

pub struct Activation {
    pub registrations: Vec<opencode_location::state::Registration>,
    pub instruction: config::instruction::ConfigInstructionHandle,
    pub reference: config::reference::ConfigReferenceHandle,
    pub agent: config::agent::ConfigAgentHandle,
    pub command: config::command::ConfigCommandHandle,
    pub compatibility: config::compatibility::ConfigCompatibilityHandle,
    pub skill: config::skill::ConfigSkillHandle,
    pub policy: config::policy::ConfigPolicyHandle,
}

pub async fn activate(deps: Deps) -> Activation {
    let entries = deps.config.entries();
    let app = deps.app.clone();
    let env = deps.env.clone();
    let plan_home = deps.home.clone();
    let command_directory = deps.directory.clone();

    let registrations = vec![
        deps.agents.transform(agent::agent).await,
        deps.agents.transform(move |editor| agent::plan(editor, &plan_home)).await,
        deps.commands.transform(move |editor| command::command(editor, &command_directory)).await,
        deps.skills.transform(move |editor| skill::skill(editor, &app, env.as_ref(), &entries)).await,
    ];

    let instruction = config::instruction::register(
        &deps.config,
        &deps.instructions,
        config::instruction::InstructionEnvironment {
            directory: deps.directory.clone(),
            project_directory: deps.project_directory,
            home: deps.home.clone(),
            config_directory: deps.config_directory,
        },
    )
    .await;

    let reference = config::reference::register(
        &deps.config,
        &deps.references,
        config::reference::ReferenceEnvironment { directory: deps.directory.clone(), home: deps.home.clone() },
    )
    .await;

    let agent = config::agent::register(&deps.config, &deps.agents, &deps.home).await;
    let command = config::command::register(&deps.config, &deps.commands, &deps.directory).await;
    let compatibility = config::compatibility::register(&deps.config, &deps.skills).await;

    let skill = config::skill::register(
        &deps.config,
        &deps.skills,
        config::skill::SkillEnvironment { directory: deps.directory, home: deps.home, cache: deps.cache },
    )
    .await;

    let policy = config::policy::register(&deps.config, &deps.skills).await;

    Activation { registrations, instruction, reference, agent, command, compatibility, skill, policy }
}
