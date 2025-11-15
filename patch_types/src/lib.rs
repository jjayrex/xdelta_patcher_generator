use serde::{Serialize, Deserialize};

#[derive(Serialize, Deserialize, Debug)]
pub struct Manifest {
    pub product: String,
    pub from_version: String,
    pub to_version: String,
    pub files: Vec<FileEntry>,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct FileEntry {
    pub path: String,
    pub kind: PatchKind,
    pub original_hash: [u8; 32],
    pub new_hash: [u8; 32],
}

#[derive(Serialize, Deserialize, Debug)]
pub enum PatchKind {
    Unchanged,
    Patched { idx: usize },
    Added { idx: usize },
    Deleted,
}

#[derive(Serialize, Deserialize, Debug)]
pub enum PatchData {
    Xdelta(Vec<u8>),    // xdelta diff
    Full(Vec<u8>),      // full file
}

#[derive(Serialize, Deserialize, Debug)]
pub struct PatchBundle {
    pub manifest: Manifest,
    pub entries: Vec<PatchData>,
}