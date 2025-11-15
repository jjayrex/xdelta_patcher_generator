use bincode::{Encode, Decode};

#[derive(Encode, Decode)]
pub struct Manifest {
    pub product: String,
    pub from_version: String,
    pub to_version: String,
    pub files: Vec<FileEntry>,
}

#[derive(Encode, Decode)]
pub struct FileEntry {
    pub path: String,
    pub kind: PatchKind,
    pub original_hash: [u8; 32],
    pub new_hash: [u8; 32],
}

#[derive(Encode, Decode)]
pub enum PatchKind {
    Unchanged,
    Patched { idx: usize },
    Added { idx: usize },
    Deleted,
}

#[derive(Encode, Decode)]
pub enum PatchData {
    Xdelta(Vec<u8>), // xdelta diff
    Full(Vec<u8>),   // full file
}

#[derive(Encode, Decode)]
pub struct PatchBundle {
    pub manifest: Manifest,
    pub entries: Vec<PatchData>,
}
