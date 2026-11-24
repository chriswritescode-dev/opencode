#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProjectionSettings {
    pub part_rows: bool,
    pub media_blob: bool,
    pub fork_media: bool,
}

impl Default for ProjectionSettings {
    fn default() -> Self {
        ProjectionSettings { part_rows: true, media_blob: false, fork_media: false }
    }
}
