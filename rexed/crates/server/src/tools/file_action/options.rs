//! File mutation input contracts.
use serde::Deserialize;
use std::path::PathBuf;

#[derive(Clone, Debug, Deserialize)]
pub struct FileActionOptions {
    pub mode: FileActionMode,
    #[serde(rename = "filePath")]
    pub file_path: PathBuf,
    #[serde(default, rename = "newFilePath")]
    pub new_file_path: Option<PathBuf>,
    #[serde(default, rename = "patchText")]
    pub patch_text: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default, rename = "patchMode")]
    pub patch_mode: PatchMode,
    #[serde(default, rename = "hashCheckMode")]
    pub hash_check_mode: bool,
    #[serde(default, rename = "hashCode")]
    pub hash_code: Option<String>,
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum FileActionMode {
    Patch,
    Create,
    Delete,
    Rename,
}
#[derive(Clone, Copy, Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum PatchMode {
    #[default]
    Text,
    Binary,
}
