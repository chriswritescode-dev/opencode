use std::sync::Arc;

use crate::access::Access;
use crate::filesystem::LocationFileSystem;
use crate::formatter::Formatter;
use crate::mutation::Mutation;
use crate::search::Search;
use crate::write::FileWriter;

pub struct LocationFiles {
    pub filesystem: LocationFileSystem,
    pub access: Access,
    pub mutation: Mutation,
    pub write: FileWriter,
    pub formatter: Arc<Formatter>,
    pub search: Search,
}
