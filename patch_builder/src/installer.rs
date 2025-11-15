use anyhow::Result;
use std::fs::File;
use std::io::Write;
use std::path::Path;
use bincode;
use patch_types::PatchBundle;

const PATCH_STUB_EXE: &[u8] = include_bytes!("../patch_stub.exe");

pub fn build_installer_exe(bundle: &PatchBundle, output: &Path) -> Result<()> {
    let mut out = File::create(output)?;

    // Write stub
    out.write_all(PATCH_STUB_EXE)?;

    // Serialize bundle
    let bundle_bytes = bincode::encode_to_vec(bundle, bincode::config::standard())?;
    out.write_all(&bundle_bytes)?;

    // Append length footer
    let len = bundle_bytes.len() as u64;
    out.write_all(&len.to_le_bytes())?;

    Ok(())
}