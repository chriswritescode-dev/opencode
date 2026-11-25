use crate::project::Project;

#[derive(Clone, Debug, PartialEq)]
pub enum LocationEvent {
    ProjectUpdated(Project),
    WorktreeResolved { project_id: String, directory: String, previous: String, adopted: Vec<String> },
    LocationShutdown { directory: String, workspace_id: Option<String> },
    ConfigUpdated { directory: String, workspace_id: Option<String> },
}
