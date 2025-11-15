use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;
use std::sync::Arc;

use anyhow::{Context, Result};
use bincode;
use blake3;
use indicatif::{ProgressBar, ProgressStyle, MultiProgress, ProgressState};
use rayon::prelude::*;
use rayon::{current_num_threads, current_thread_index};
use xdelta3;

use patch_types::{PatchBundle, PatchData, PatchKind};

fn main() -> Result<()> {
    let bundle = load_bundle()?;
    let cwd = std::env::current_dir()?;

    verify_base_folder(&bundle, &cwd)?;
    apply_bundle(&bundle, &cwd)?;
    Ok(())
}

fn load_bundle() -> Result<PatchBundle> {
    let exe = std::env::current_exe()?;
    let mut file = File::open(exe)?;
    let len = file.metadata()?.len();
    if len < 8 {
        anyhow::bail!("Invalid patch exe (too small)");
    }

    // Read footer
    file.seek(SeekFrom::End(-8))?;
    let mut footer = [0u8; 8];
    file.read_exact(&mut footer)?;
    let bundle_len = u64::from_le_bytes(footer);
    if bundle_len + 8 > len {
        anyhow::bail!("Invalid bundle length");
    }

    // Read bundle
    file.seek(SeekFrom::Start(len - 8 - bundle_len))?;
    let mut buffer = vec![0u8; bundle_len as usize];
    file.read_exact(&mut buffer)?;

    let bundle: PatchBundle =
        bincode::borrow_decode_from_slice(&buffer, bincode::config::standard())?.0;
    Ok(bundle)
}

fn hash_file(path: &Path) -> Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    let mut file = File::open(path)?;
    let mut buffer = [0u8; 8192];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(*hasher.finalize().as_bytes())
}

fn verify_base_folder(bundle: &PatchBundle, cwd: &Path) -> Result<()> {
    for file in &bundle.manifest.files {
        match file.kind {
            PatchKind::Unchanged | PatchKind::Patched { .. } | PatchKind::Deleted => {
                if file.original_hash != [0u8; 32] {
                    let path = cwd.join(&file.path);
                    if !path.exists() {
                        anyhow::bail!("Expected file missing: {}", file.path);
                    }
                    let hash =
                        hash_file(&path).with_context(|| format!("Hashing {}", file.path))?;
                    if hash != file.original_hash {
                        anyhow::bail!("File {} hash mismatch", file.path);
                    }
                }
            }
            PatchKind::Added { .. } => {}
        }
    }
    Ok(())
}

fn apply_bundle(bundle: &PatchBundle, cwd: &Path) -> Result<()> {
    let total_files = bundle.manifest.files.len() as u64;

    let mp = Arc::new(MultiProgress::new());

    let overall_pb = mp.add(ProgressBar::new(total_files));
    overall_pb.set_style(
        ProgressStyle::with_template(
            "[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}",
        )?
            .progress_chars("##-"),
    );
    overall_pb.set_message("Patching files");

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

    let base_dir = cwd.to_path_buf();
    let entries = &bundle.entries;
    let files = &bundle.manifest.files;

    files.par_iter().try_for_each(|file| {
        let base = base_dir.clone();
        let entries = entries;
        let overall_pb = overall_pb.clone();
        let worker_bars = worker_bars.clone();

        let idx = current_thread_index().unwrap_or(0);
        let worker_pb = &worker_bars[idx];

        let target = base.join(&file.path);

        match file.kind {
            PatchKind::Unchanged => {
                worker_pb.set_length(1);
                worker_pb.set_position(1);
            }
            PatchKind::Deleted => {
                let len = std::fs::metadata(&target).map(|m| m.len()).unwrap_or(1);
                worker_pb.set_length(len);
                if target.exists() {
                    fs::remove_file(&target).with_context(|| format!("Removing {}", file.path))?;
                }
                worker_pb.set_position(len);
            }
            PatchKind::Added { idx } => {
                let data = entries
                    .get(idx)
                    .ok_or_else(|| anyhow::anyhow!("Invalid entry index for {}", file.path))?;

                let bytes = match data {
                    PatchData::Full(b) => b,
                    _ => anyhow::bail!("'Added' has wrong PatchData type for {}", file.path),
                };

                if let Some(parent) = target.parent() {
                    fs::create_dir_all(parent)
                        .with_context(|| format!("Creating dir for {}", file.path))?;
                }

                let total = bytes.len() as u64;
                worker_pb.set_length(total);

                let mut tmp = target.clone();
                tmp.set_extension("tmp");


                let mut out = File::create(&tmp)
                    .with_context(|| format!("Creating temp for {}", file.path))?;

                let mut written: u64 = 0;
                for chunk in bytes.chunks(8192) {
                    out.write_all(chunk).with_context(|| format!("Writing {}", file.path))?;
                    written += chunk.len() as u64;
                    worker_pb.set_position(written);
                }

                fs::rename(&tmp, &target).with_context(|| format!("Renaming {}", file.path))?;
            }
            PatchKind::Patched { idx } => {
                let data = entries
                    .get(idx)
                    .ok_or_else(|| anyhow::anyhow!("Invalid entry index for {}", file.path))?;

                let patch = match data {
                    PatchData::Xdelta(p) => p,
                    _ => anyhow::bail!("Patched has wrong PatchData type for {}", file.path),
                };

                let org_len = std::fs::metadata(&target).with_context(|| format!("Metadata for {}", file.path))?.len();
                worker_pb.set_length(org_len);

                let mut org_bytes = Vec::with_capacity(org_len as usize);
                let mut org_file = File::open(&target).with_context(|| format!("Opening {}", file.path))?;
                let mut buffer = [0u8; 8192];
                let mut read_total: u64 = 0;

                loop {
                    let n = org_file.read(&mut buffer)
                        .with_context(|| format!("Reading original {}", file.path))?;
                    if n == 0 {
                        break;
                    }
                    org_bytes.extend_from_slice(&buffer[..n]);
                    read_total += n as u64;
                    worker_pb.set_position(read_total);
                }

                let new_bytes = xdelta3::decode(patch, &org_bytes)
                    .with_context(|| format!("xdelta decode failed for {}", file.path))?;

                let new_len = new_bytes.len() as u64;
                let total = org_len + new_len;

                worker_pb.set_length(total);
                let mut pos = read_total;

                let mut tmp = target.clone();
                tmp.set_extension("tmp");

                let mut out = File::create(&tmp).with_context(|| format!("Creating temp for {}", file.path))?;

                for chunk in new_bytes.chunks(8192) {
                    out.write_all(chunk).with_context(|| format!("Writing {}", file.path))?;
                    pos += chunk.len() as u64;
                    worker_pb.set_position(pos);
                }

                fs::rename(&tmp, &target).with_context(|| format!("Renaming {}", file.path))?;
            }
        }

        overall_pb.inc(1);
        Ok::<(), anyhow::Error>(())
    })?;

    overall_pb.finish_with_message("Patching complete");

    for (i, wb) in worker_bars.iter().enumerate() {
        wb.finish_with_message(format!("Worker {i}: done"));
    }

    Ok(())
}

// fn apply_bundle(bundle: &PatchBundle, cwd: &Path) -> Result<()> {
//     let total_files = bundle.manifest.files.len() as u64;
//
//     let pb = ProgressBar::new(total_files);
//     pb.set_style(
//         ProgressStyle::with_template(
//             "[{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} {msg}"
//         )?
//         .progress_chars("##-"),
//     );
//
//     for file in &bundle.manifest.files {
//         let target = cwd.join(&file.path);
//
//         match file.kind {
//             PatchKind::Unchanged => {
//
//             },
//             PatchKind::Deleted => {
//                 if target.exists() {
//                     fs::remove_file(&target).with_context(|| format!("Removing {}", file.path))?;
//                 }
//             },
//             PatchKind::Added { idx } => {
//                 if let Some(PatchData::Full(bytes)) = bundle.entries.get(idx) {
//                     if let Some(parent) = target.parent() {
//                         fs::create_dir_all(parent)?;
//                     }
//                     let mut tmp = target.clone();
//                     tmp.set_extension("tmp");
//                     {
//                         let mut out = File::create(&tmp)?;
//                         out.write_all(bytes)?;
//                     }
//                     fs::rename(&tmp, &target)?;
//                 } else {
//                     anyhow::bail!("Invalid bundle: 'Added' has wrong data type");
//                 }
//             },
//             PatchKind::Patched { idx } => {
//                 let org_bytes = {
//                     let mut buffer = Vec::new();
//                     File::open(&target)?.read_to_end(&mut buffer)?;
//                     buffer
//                 };
//
//                 let patch = match bundle.entries.get(idx) {
//                     Some(PatchData::Xdelta(p)) => p,
//                     _ => anyhow::bail!("Invalid bundle: 'Patched' has wrong data type"),
//                 };
//
//                 let new_bytes = xdelta3::decode(patch, &org_bytes).context("xdelta decode failed")?;
//
//                 let mut tmp = target.clone();
//                 tmp.set_extension("tmp");
//                 {
//                     let mut out = File::create(&tmp)?;
//                     out.write_all(&new_bytes)?;
//                 }
//                 fs::rename(&tmp, &target)?;
//             }
//         }
//         pb.inc(1);
//     }
//     pb.finish_with_message("Patching complete");
//     Ok(())
// }
