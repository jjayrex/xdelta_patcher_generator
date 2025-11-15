mod installer;

use std::collections::HashMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use blake3::Hasher;
use clap::Parser;
use patch_types::{FileEntry, Manifest, PatchBundle, PatchData, PatchKind};
use path_slash::PathExt as _;
use walkdir::WalkDir;
use crate::installer::build_installer_exe;

#[derive(Parser)]
struct Args {
    /// Folder with the old version
    old_dir: PathBuf,
    /// Folder with the new version
    new_dir: PathBuf,
    /// Output patch executable
    output: PathBuf,
    /// Product name
    #[arg(long)]
    product: String,
    /// From Version String
    #[arg(long)]
    from_version: String,
    /// To Version String
    #[arg(long)]
    to_version: String,
    /// If set, delete files that exist in old_dir but are not present in new_dir
    #[arg(short = 'd', long)]
    delete_extra: bool,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let bundle = build_bundle(&args.old_dir, &args.new_dir, &args.product, &args.from_version, &args.to_version, args.delete_extra)?;
    build_installer_exe(&bundle, &args.output)?;
    Ok(())
}

fn hash_file(path: &Path) -> Result<[u8; 32]> {
    let mut hasher = Hasher::new();
    let mut file = File::open(path)?;
    let mut buffer = [0u8; 32];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn build_bundle(
    old_dir: &Path,
    new_dir: &Path,
    product: &str,
    from_version: &str,
    to_version: &str,
    delete_extra: bool,
) -> Result<PatchBundle> {
    let mut entries = Vec::<PatchData>::new();
    let mut files = Vec::<FileEntry>::new();

    // Index old files
    let mut old_map: HashMap<String, PathBuf> = HashMap::new();
    for entry in WalkDir::new(old_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let rel = entry.path().strip_prefix(old_dir)?;
        let rel_str = rel.to_slash().unwrap().into_owned();
        old_map.insert(rel_str, entry.into_path());
    }

    // Iterate new files and compare
    for entry in WalkDir::new(new_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let new_path = entry.path().to_path_buf();
        let rel = new_path.strip_prefix(new_dir)?;
        let rel_str = rel.to_slash().unwrap().into_owned();

        let new_hash = hash_file(&new_path)?;
        if let Some(old_path) = old_map.remove(&rel_str) {
            let old_hash = hash_file(&old_path)?;
            if old_hash == new_hash {
                files.push(FileEntry {
                    path: rel_str,
                    kind: PatchKind::Unchanged,
                    original_hash: old_hash,
                    new_hash,
                });
            } else {
                // Create xdelta patch
                let patch_data = create_patch(&old_path, &new_path)?;
                let idx = entries.len();
                entries.push(PatchData::Xdelta(patch_data));
                files.push(FileEntry {
                    path: rel_str,
                    kind: PatchKind::Patched { idx },
                    original_hash: old_hash,
                    new_hash,
                });
            }
        } else {
            // New file
            let mut buffer = Vec::new();
            File::open(&new_path)?.read_to_end(&mut buffer)?;
            let idx = entries.len();
            entries.push(PatchData::Full(buffer));
            files.push(FileEntry {
                path: rel_str,
                kind: PatchKind::Added { idx },
                original_hash: [0u8; 32],
                new_hash,
            });
        }
    }

    // Deleted files
    if delete_extra {
        for (rel_str, old_path) in old_map {
            let old_hash = hash_file(&old_path)?;
            files.push(FileEntry {
                path: rel_str,
                kind: PatchKind::Deleted,
                original_hash: old_hash,
                new_hash: [0u8; 32],
            });
        }
    }

    let manifest = Manifest {
        product: product.to_string(),
        from_version: from_version.to_string(),
        to_version: to_version.to_string(),
        files,
    };

    Ok(PatchBundle { manifest, entries })
}

fn create_patch(old_path: &Path, new_path: &Path) -> Result<Vec<u8>> {
    let mut old = Vec::new();
    let mut new_ = Vec::new();
    File::open(old_path)?.read_to_end(&mut old)?;
    File::open(new_path)?.read_to_end(&mut new_)?;

    let patch = xdelta3::encode(&new_, &old).context("xdelta encode failed")?;
    Ok(patch)
}
