use std::sync::{Arc, RwLock};

use opencode_config::entry::Entry;
use opencode_contract::generated::PermissionRule;
use opencode_fs::LocationFiles;
use opencode_fs::access::{Access, Authorizer};
use opencode_fs::formatter::Formatter;
use opencode_fs::mutation::Mutation;
use opencode_fs::ripgrep::Ripgrep;
use opencode_fs::write::{FileWriter, FormatHook};
use opencode_location::state::StateError;
use opencode_registry::skill::SkillService;

use crate::ToolRegistry;
use crate::builtin::{self, BuiltinDeps};
use crate::image::Image;
use crate::output::OutputStore;
use crate::snapshot::{Snapshot, SnapshotOptions};
use crate::tools::edit::EditDeps;
use crate::tools::glob::GlobDeps;
use crate::tools::grep::GrepDeps;
use crate::tools::patch::PatchDeps;
use crate::tools::read::{NearbyInstructions, ReadDeps};
use crate::tools::read_filesystem::LocalFiles;
use crate::tools::skill::SkillDeps;
use crate::tools::write::WriteDeps;

pub struct LocationToolDeps {
    pub files: LocationFilesParts,
    pub ripgrep: Arc<Ripgrep>,
    pub skills: SkillService,
    pub instructions: Arc<dyn NearbyInstructions>,
    pub authorizer: Arc<dyn Authorizer>,
    pub output: OutputStore,
    pub image: Arc<Image>,
    pub claude: bool,
    pub full_codemode: bool,
}

pub struct LocationFilesParts {
    pub access: Access,
    pub mutation: Mutation,
    pub write: FileWriter,
    pub formatter: Arc<Formatter>,
}

impl LocationFilesParts {
    pub fn new(files: &LocationFiles) -> Self {
        LocationFilesParts {
            access: files.access.clone(),
            mutation: files.mutation.clone(),
            write: files.write.clone(),
            formatter: files.formatter.clone(),
        }
    }
}

pub struct LocationTools {
    registry: ToolRegistry,
    _registration: opencode_location::state::Registration,
    authorizer: Arc<dyn Authorizer>,
    image: Arc<Image>,
    output: RwLock<OutputStore>,
    full_codemode: bool,
}

impl LocationTools {
    pub async fn new(deps: LocationToolDeps) -> Self {
        let formatter: Arc<dyn FormatHook> = deps.files.formatter;
        let registry = ToolRegistry::new();
        let builtins = BuiltinDeps {
            full_codemode: deps.full_codemode,
            read: Some(ReadDeps {
                files: Arc::new(LocalFiles),
                access: deps.files.access.clone(),
                skills: Some(deps.skills.clone()),
                instructions: deps.instructions,
                claude: deps.claude,
            }),
            write: Some(WriteDeps { writer: deps.files.write.clone() }),
            edit: Some(EditDeps { writer: deps.files.write, mutation: deps.files.mutation.clone() }),
            patch: Some(PatchDeps::new(
                deps.files.access.clone(),
                deps.files.mutation.clone(),
                deps.authorizer.clone(),
                formatter,
            )),
            glob: Some(GlobDeps { ripgrep: deps.ripgrep.clone(), access: deps.files.access.clone() }),
            grep: Some(GrepDeps { ripgrep: deps.ripgrep, access: deps.files.access }),
            skill: Some(SkillDeps { skills: deps.skills }),
        };
        let registration = builtin::activate(&registry, &builtins).await;
        LocationTools {
            registry,
            _registration: registration,
            authorizer: deps.authorizer,
            image: deps.image,
            output: RwLock::new(deps.output),
            full_codemode: deps.full_codemode,
        }
    }

    pub fn registry(&self) -> &ToolRegistry {
        &self.registry
    }

    pub fn authorizer(&self) -> Arc<dyn Authorizer> {
        self.authorizer.clone()
    }

    pub fn image(&self) -> &Arc<Image> {
        &self.image
    }

    pub fn output(&self) -> OutputStore {
        self.output.read().expect("the output store lock is not poisoned").clone()
    }

    pub fn full_codemode(&self) -> bool {
        self.full_codemode
    }

    pub fn configure(&self, entries: &[Entry]) {
        self.image.configure(entries);
        self.output.write().expect("the output store lock is not poisoned").observe(entries);
    }

    pub async fn snapshot(
        &self,
        permissions: &[PermissionRule],
        options: &SnapshotOptions,
    ) -> Result<Snapshot, StateError> {
        let mut snapshot = self.registry.snapshot(permissions, options).await?;
        snapshot.image = self.image.clone();
        Ok(snapshot)
    }
}
