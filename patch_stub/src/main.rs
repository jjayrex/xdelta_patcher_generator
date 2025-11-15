use std::fs::{self, File};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::Path;

use anyhow::{Context, Result};
use blake3;
use xdelta3;
use bincode;
use patch_types::{PatchBundle, PatchData, PatchKind};

fn main() -> Result<()> {
    let bundle = load_bundle()?;
    let cwd = std::env::current_dir()?;

    verify_base_folder(&bundle, &cwd)?;
    apply_bundle(&bundle, &cwd)?;

    println!(
        "Patched {} from {} to {}",
        bundle.manifest.product, bundle.manifest.from_version, bundle.manifest.to_version
    );
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

    let bundle: PatchBundle = bincode::borrow_decode_from_slice(&buffer, bincode::config::standard())?.0;
    Ok(bundle)
}

fn hash_file(path: &Path) -> Result<[u8; 32]> {
    let mut hasher = blake3::Hasher::new();
    let mut file = File::open(path)?;
    let mut buffer = [0u8; 8192];
    loop {
        let n = file.read(&mut buffer)?;
        if n == 0 { break; }
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
                    let hash = hash_file(&path).with_context(|| format!("Hashing {}", file.path))?;
                    if hash != file.original_hash {
                        anyhow::bail!("File {} hash mismatch", file.path);
                    }
                }
            }
            PatchKind::Added { .. } => {

            }
        }
    }
    Ok(())
}

fn apply_bundle(bundle: &PatchBundle, cwd: &Path) -> Result<()> {
    for file in &bundle.manifest.files {
        let target = cwd.join(&file.path);

        match file.kind {
            PatchKind::Unchanged => {

            },
            PatchKind::Deleted => {
                if target.exists() {
                    fs::remove_file(&target).with_context(|| format!("Removing {}", file.path))?;
                }
            },
            PatchKind::Added { idx } => {
                if let Some(PatchData::Full(bytes)) = bundle.entries.get(idx) {
                    if let Some(parent) = target.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    let mut tmp = target.clone();
                    tmp.set_extension("tmp");
                    {
                        let mut out = File::create(&tmp)?;
                        out.write_all(bytes)?;
                    }
                    fs::rename(&tmp, &target)?;
                } else {
                    anyhow::bail!("Invalid bundle: 'Added' has wrong data type");
                }
            },
            PatchKind::Patched { idx } => {
                let org_bytes = {
                    let mut buffer = Vec::new();
                    File::open(&target)?.read_to_end(&mut buffer)?;
                    buffer
                };

                let patch = match bundle.entries.get(idx) {
                    Some(PatchData::Xdelta(p)) => p,
                    _ => anyhow::bail!("Invalid bundle: 'Patched' has wrong data type"),
                };

                let new_bytes = xdelta3::decode(patch, &org_bytes).context("xdelta decode failed")?;

                let mut tmp = target.clone();
                tmp.set_extension("tmp");
                {
                    let mut out = File::create(&tmp)?;
                    out.write_all(&new_bytes)?;
                }
                fs::rename(&tmp, &target)?;
            }
        }
    }
    Ok(())
}