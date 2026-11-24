use std::path::PathBuf;

use opencode_contract::codec::{decode_json, encode_json};
use opencode_contract::generated::{
    AbsolutePath, ConfigEntry, ConfigEntryDirectory, ConfigEntryDirectoryType, ConfigEntryDocument,
    ConfigEntryDocumentInfo, ConfigEntryDocumentType,
};

#[allow(clippy::large_enum_variant)]
#[derive(Clone, Debug, PartialEq)]
pub enum Entry {
    Document(Document),
    Directory { path: PathBuf },
}

#[derive(Clone, Debug, PartialEq)]
pub struct Document {
    pub path: Option<PathBuf>,
    pub info: ConfigEntryDocumentInfo,
    pub origin: Origin,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Origin {
    File,
    Managed,
    Integration { integration_id: String, connection: serde_json::Value, organization: Option<String> },
    WellKnown { integration_id: String },
}

impl Entry {
    pub fn to_wire(&self) -> ConfigEntry {
        match self {
            Self::Document(document) => ConfigEntry::Document(ConfigEntryDocument {
                r#type: ConfigEntryDocumentType::Document,
                path: document.path.as_ref().map(|path| AbsolutePath(path.to_string_lossy().into_owned())),
                info: document.info.clone(),
            }),
            Self::Directory { path } => ConfigEntry::Directory(ConfigEntryDirectory {
                r#type: ConfigEntryDirectoryType::Directory,
                path: AbsolutePath(path.to_string_lossy().into_owned()),
            }),
        }
    }
}

pub fn latest<T>(entries: &[Entry], pick: impl Fn(&ConfigEntryDocumentInfo) -> Option<T>) -> Option<T> {
    entries.iter().rev().find_map(|entry| match entry {
        Entry::Document(document) => pick(&document.info),
        Entry::Directory { .. } => None,
    })
}

impl Document {
    pub fn policies_only(self) -> Self {
        let text = match &self.info.policies {
            Some(policies) => format!(r#"{{"policies":{}}}"#, encode_json(policies)),
            None => "{}".to_owned(),
        };
        Self {
            info: decode_json::<ConfigEntryDocumentInfo>(&text).expect("a document holding only policies decodes"),
            ..self
        }
    }
}
