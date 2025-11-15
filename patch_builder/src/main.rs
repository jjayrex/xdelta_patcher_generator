use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use anyhow::{ Context, Result };
use clap::Parser;
use walkdir::WalkDir;
use blake3::Hasher;
use xdelta3;
use patch_types::{ PatchBundle, Manifest, FileEntry, PatchKind, PatchData };

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
}

fn main() -> Result<()> {
    let args = Args::parse();
    
}