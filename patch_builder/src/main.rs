mod installer;

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use blake3::Hasher;
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle};
use path_slash::PathExt as _;
use rayon::prelude::*;
use walkdir::WalkDir;

use crate::installer::build_installer_exe;
use patch_types::{FileEntry, Manifest, PatchBundle, PatchData, PatchKind};

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

#[derive(Clone)]
struct FileRec {
    rel: String,
    path: PathBuf,
}

enum TempKind {
    Unchanged,
    Added(PatchData),
    Patched(PatchData),
}

struct TempResult {
    path: String,
    original_hash: [u8; 32],
    new_hash: [u8; 32],
    kind: TempKind,
}

fn main() -> Result<()> {
    let args = Args::parse();
    let bundle = build_bundle(
        &args.old_dir,
        &args.new_dir,
        &args.product,
        &args.from_version,
        &args.to_version,
        args.delete_extra,
    )?;
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
    // Collect file lists
    let mut old_files = Vec::<FileRec>::new();
    for entry in WalkDir::new(old_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let rel = entry.path().strip_prefix(old_dir)?;
        let rel_str = rel.to_slash().unwrap().to_string();
        old_files.push(FileRec {
            rel: rel_str,
            path: entry.into_path(),
        });
    }

    let mut new_files = Vec::<FileRec>::new();
    for entry in WalkDir::new(new_dir)
        .into_iter()
        .filter_map(Result::ok)
        .filter(|e| e.file_type().is_file())
    {
        let rel = entry.path().strip_prefix(new_dir)?;
        let rel_str = rel.to_slash().unwrap().to_string();
        new_files.push(FileRec {
            rel: rel_str,
            path: entry.into_path(),
        });
    }

    // Index old files & record new paths
    let old_map: HashMap<String, PathBuf> = old_files
        .iter()
        .map(|r| (r.rel.clone(), r.path.clone()))
        .collect();
    let new_set: HashSet<String> = new_files.iter().map(|r| r.rel.clone()).collect();

    // Progress bar
    let total_tasks = new_files.len()
        + if delete_extra {
            old_files
                .iter()
                .filter(|rec| !new_set.contains(&rec.rel))
                .count()
        } else {
            0
        };

    let pb = Arc::new(ProgressBar::new(total_tasks as u64));
    pb.set_style(
        ProgressStyle::with_template(
            "[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}",
        )?
            .progress_chars("##-"),
    );


    // Process new files
    let old_map_arc = Arc::new(old_map);
    let temp_results: Result<Vec<TempResult>> = new_files
        .par_iter()
        .map(|rec| {
            let pb = pb.clone();
            let old_map = old_map_arc.clone();

            pb.set_message(format!("Scanning {}", rec.rel));

            let new_hash = hash_file(&rec.path)?;

            let res = if let Some(old_path) = old_map.get(&rec.rel) {
                let old_hash = hash_file(old_path)?;

                if old_hash == new_hash {
                    // unchanged
                    TempResult {
                        path: rec.rel.clone(),
                        original_hash: old_hash,
                        new_hash,
                        kind: TempKind::Unchanged,
                    }
                } else {
                    // changed
                    let patch_data = create_patch(old_path, &rec.path)?;
                    TempResult {
                        path: rec.rel.clone(),
                        original_hash: old_hash,
                        new_hash,
                        kind: TempKind::Patched(PatchData::Xdelta(patch_data)),
                    }
                }
            } else {
                // added
                let mut buffer = Vec::new();
                File::open(&rec.path)?.read_to_end(&mut buffer)?;
                TempResult {
                    path: rec.rel.clone(),
                    original_hash: [0u8; 32],
                    new_hash,
                    kind: TempKind::Added(PatchData::Full(buffer)),
                }
            };

            pb.inc(1);
            Ok::<TempResult, anyhow::Error>(res)
        })
        .collect();

    let temp_results = temp_results?;

    // Delete extra files if --delete-extra was used
    let deleted_entries: Vec<FileEntry> = if delete_extra {
        old_files
            .par_iter()
            .filter(|rec| !new_set.contains(&rec.rel))
            .map(|rec| {
                let pb = pb.clone();
                pb.set_message(format!("Marking deleted {}", rec.rel));

                let old_hash = hash_file(&rec.path)?;
                pb.inc(1);

                Ok::<FileEntry, anyhow::Error>(FileEntry {
                    path: rec.rel.clone(),
                    kind: PatchKind::Deleted,
                    original_hash: old_hash,
                    new_hash: [0u8; 32],
                })
            })
            .collect::<Result<Vec<_>>>()?
    } else {
        Vec::new()
    };

    // Final assembly
    let mut entries_vec = Vec::<PatchData>::new();
    let mut files_vec = Vec::<FileEntry>::new();

    for r in temp_results {
        match r.kind {
            TempKind::Unchanged => {
                files_vec.push(FileEntry {
                    path: r.path,
                    kind: PatchKind::Unchanged,
                    original_hash: r.original_hash,
                    new_hash: r.new_hash,
                });
            }
            TempKind::Added(patch_data) => {
                let idx = entries_vec.len();
                entries_vec.push(patch_data);
                files_vec.push(FileEntry {
                    path: r.path,
                    kind: PatchKind::Added { idx },
                    original_hash: r.original_hash,
                    new_hash: r.new_hash,
                });
            }
            TempKind::Patched(patch_data) => {
                let idx = entries_vec.len();
                entries_vec.push(patch_data);
                files_vec.push(FileEntry {
                    path: r.path,
                    kind: PatchKind::Patched { idx },
                    original_hash: r.original_hash,
                    new_hash: r.new_hash,
                });
            }
        }
    }

    files_vec.extend(deleted_entries);

    pb.finish_with_message("Bundle build complete");

    let manifest = Manifest {
        product: product.to_string(),
        from_version: from_version.to_string(),
        to_version: to_version.to_string(),
        files: files_vec,
    };

    Ok(PatchBundle {
        manifest,
        entries: entries_vec,
    })
}

fn create_patch(old_path: &Path, new_path: &Path) -> Result<Vec<u8>> {
    let mut old = Vec::new();
    let mut new_ = Vec::new();
    File::open(old_path)?.read_to_end(&mut old)?;
    File::open(new_path)?.read_to_end(&mut new_)?;

    let patch = xdelta3::encode(&new_, &old).context("xdelta encode failed")?;
    Ok(patch)
}

// fn build_bundle(
//     old_dir: &Path,
//     new_dir: &Path,
//     product: &str,
//     from_version: &str,
//     to_version: &str,
//     delete_extra: bool,
// ) -> Result<PatchBundle> {
//     let mut entries = Vec::<PatchData>::new();
//     let mut files = Vec::<FileEntry>::new();
//
//     // Index old files
//     let mut old_map: HashMap<String, PathBuf> = HashMap::new();
//     for entry in WalkDir::new(old_dir)
//         .into_iter()
//         .filter_map(Result::ok)
//         .filter(|e| e.file_type().is_file())
//     {
//         let rel = entry.path().strip_prefix(old_dir)?;
//         let rel_str = rel.to_slash().unwrap().into_owned();
//         old_map.insert(rel_str, entry.into_path());
//     }
//
//     // Iterate new files and compare
//     for entry in WalkDir::new(new_dir)
//         .into_iter()
//         .filter_map(Result::ok)
//         .filter(|e| e.file_type().is_file())
//     {
//         let new_path = entry.path().to_path_buf();
//         let rel = new_path.strip_prefix(new_dir)?;
//         let rel_str = rel.to_slash().unwrap().into_owned();
//
//         let new_hash = hash_file(&new_path)?;
//         if let Some(old_path) = old_map.remove(&rel_str) {
//             let old_hash = hash_file(&old_path)?;
//             if old_hash == new_hash {
//                 files.push(FileEntry {
//                     path: rel_str,
//                     kind: PatchKind::Unchanged,
//                     original_hash: old_hash,
//                     new_hash,
//                 });
//             } else {
//                 // Create xdelta patch
//                 let patch_data = create_patch(&old_path, &new_path)?;
//                 let idx = entries.len();
//                 entries.push(PatchData::Xdelta(patch_data));
//                 files.push(FileEntry {
//                     path: rel_str,
//                     kind: PatchKind::Patched { idx },
//                     original_hash: old_hash,
//                     new_hash,
//                 });
//             }
//         } else {
//             // New file
//             let mut buffer = Vec::new();
//             File::open(&new_path)?.read_to_end(&mut buffer)?;
//             let idx = entries.len();
//             entries.push(PatchData::Full(buffer));
//             files.push(FileEntry {
//                 path: rel_str,
//                 kind: PatchKind::Added { idx },
//                 original_hash: [0u8; 32],
//                 new_hash,
//             });
//         }
//     }
//
//     // Deleted files
//     if delete_extra {
//         for (rel_str, old_path) in old_map {
//             let old_hash = hash_file(&old_path)?;
//             files.push(FileEntry {
//                 path: rel_str,
//                 kind: PatchKind::Deleted,
//                 original_hash: old_hash,
//                 new_hash: [0u8; 32],
//             });
//         }
//     }
//
//     let manifest = Manifest {
//         product: product.to_string(),
//         from_version: from_version.to_string(),
//         to_version: to_version.to_string(),
//         files,
//     };
//
//     Ok(PatchBundle { manifest, entries })
// }
