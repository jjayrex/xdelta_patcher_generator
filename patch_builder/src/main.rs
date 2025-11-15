mod installer;

use std::collections::{HashMap, HashSet};
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use clap::Parser;
use indicatif::{ProgressBar, ProgressStyle, MultiProgress, ProgressState};
use path_slash::PathExt as _;
use rayon::prelude::*;
use rayon::{current_num_threads, current_thread_index};
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

fn hash_file(path: &Path, worker_bars: &Arc<Vec<ProgressBar>>) -> Result<[u8; 32]> {
    // Identify worker
    let idx = current_thread_index().unwrap_or(0);
    let bar = &worker_bars[idx];

    let len = std::fs::metadata(path)?.len();

    bar.set_length(len);
    bar.set_position(0);

    let mut hasher = blake3::Hasher::new();
    let mut file = File::open(path)?;
    let mut buffer = [0u8; 8192];
    let mut read_total = 0u64;

    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
        read_total += n as u64;
        bar.set_position(read_total);
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

    // Progress bars
    let total_tasks = new_files.len()
        + if delete_extra {
            old_files
                .iter()
                .filter(|rec| !new_set.contains(&rec.rel))
                .count()
        } else {
            0
        };

    let mp = Arc::new(MultiProgress::new());

    let overall_pb = mp.add(ProgressBar::new(total_tasks as u64));
    overall_pb.set_style(
        ProgressStyle::with_template(
            "[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}",
        )?
            .progress_chars("##-"),
    );

    let num_workers = current_num_threads();

    let mut worker_vec = Vec::with_capacity(num_workers);
    for i in 0..num_workers {
        let pb = mp.add(ProgressBar::new(0));

        let template = format!("  [W{:02}] {{bar:30.green/black}} {{bytes}}/{{total_bytes}}", i);
        pb.set_style(
            ProgressStyle::with_template(&template)?
                .with_key("bytes", |st: &ProgressState, w: &mut dyn std::fmt::Write| {
                    write!(w, "{}", indicatif::HumanBytes(st.pos())).ok();
                })
                .with_key("total_bytes", |st: &ProgressState, w: &mut dyn std::fmt::Write| {
                    write!(w, "{}", indicatif::HumanBytes(st.len().unwrap_or(0))).ok();
                })
                .progress_chars("##-"),
        );
        worker_vec.push(pb);
    }
    let worker_bars = Arc::new(worker_vec);

    // Process new files
    let old_map_arc = Arc::new(old_map);
    let overall_pb = overall_pb.clone();
    let worker_bars_clone = worker_bars.clone();

    let temp_results: Result<Vec<TempResult>> = new_files
        .par_iter()
        .map(|rec| {
            let overall_pb = overall_pb.clone();
            let old_map = old_map_arc.clone();
            let worker_bars = worker_bars_clone.clone();

            let new_hash = hash_file(&rec.path, &worker_bars)?;

            let res = if let Some(old_path) = old_map.get(&rec.rel) {
                let old_hash = hash_file(old_path, &worker_bars)?;

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

            overall_pb.inc(1);
            Ok::<TempResult, anyhow::Error>(res)
        })
        .collect();

    let temp_results = temp_results?;

    // Delete extra files if --delete-extra was used
    let deleted_entries: Vec<FileEntry> = if delete_extra {
        let worker_bars = worker_bars.clone();
        old_files
            .par_iter()
            .filter(|rec| !new_set.contains(&rec.rel))
            .map(|rec| {
                let worker_bars = worker_bars.clone();

                let old_hash = hash_file(&rec.path, &worker_bars)?;
                overall_pb.inc(1);

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

    overall_pb.finish_with_message("Bundle build complete");

    for (i, wb) in worker_bars.iter().enumerate() {
        wb.finish_with_message(format!("Worker {i}: done"));
    }

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
//
// fn hash_file(path: &Path) -> Result<[u8; 32]> {
//     let mut hasher = Hasher::new();
//     let mut file = File::open(path)?;
//     let mut buffer = [0u8; 32];
//
//     loop {
//         let n = file.read(&mut buffer)?;
//         if n == 0 {
//             break;
//         }
//         hasher.update(&buffer[..n]);
//     }
//     Ok(*hasher.finalize().as_bytes())
// }