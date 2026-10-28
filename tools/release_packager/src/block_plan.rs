//! Block plans: which fixed-size blocks of each new release file a launcher copies from the
//! release it already runs, and which it downloads.

use std::{
    collections::HashMap,
    fs::File,
    io::{self, Read},
    path::Path,
};

use aeris_release_lifecycle::{
    BLOCK_PLAN_BLOCK_BYTES, BLOCK_PLAN_SCHEMA_VERSION, BlockDescriptor, BlockFilePlan, BlockPlan,
    MAXIMUM_BLOCK_PLAN_BLOCKS, MAXIMUM_BLOCK_PLAN_DOWNLOAD_BLOCKS, MAXIMUM_BLOCK_PLAN_FILES,
    ReleaseManifest,
};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest as _, Sha256};

/// Plans every target file that shares at least one block with the same-path source file, as
/// far as the plan's file, block and download bounds allow. A file left out downloads in full.
/// Returns `None` when no file can be planned.
pub(crate) fn plan(
    source: &ReleaseManifest,
    source_directory: &Path,
    target: &ReleaseManifest,
    target_directory: &Path,
) -> Result<Option<BlockPlan>, String> {
    let mut candidates = Vec::new();
    for target_file in &target.files {
        let Some(source_file) = source
            .files
            .iter()
            .find(|file| file.path == target_file.path)
        else {
            continue;
        };
        let source_offsets = source_block_offsets(&source_directory.join(&source_file.path))?;
        let blocks = target_blocks(&target_directory.join(&target_file.path), &source_offsets)?;
        if blocks.iter().all(|block| block.source_offset.is_none()) {
            continue;
        }
        candidates.push(BlockFilePlan {
            path: target_file.path.clone(),
            source_size: source_file.size,
            source_sha256: source_file.sha256.clone(),
            target_size: target_file.size,
            target_sha256: target_file.sha256.clone(),
            blocks,
        });
    }
    // Fewest downloads first, so the bounds admit as many files as they can.
    candidates.sort_by_key(download_blocks);
    let mut files = Vec::new();
    let (mut total_blocks, mut total_downloads) = (0_usize, 0_usize);
    for file in candidates {
        let blocks = total_blocks + file.blocks.len();
        let downloads = total_downloads + download_blocks(&file);
        if files.len() < MAXIMUM_BLOCK_PLAN_FILES
            && blocks <= MAXIMUM_BLOCK_PLAN_BLOCKS
            && downloads <= MAXIMUM_BLOCK_PLAN_DOWNLOAD_BLOCKS
        {
            total_blocks = blocks;
            total_downloads = downloads;
            files.push(file);
        }
    }
    if files.is_empty() {
        return Ok(None);
    }
    files.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(Some(BlockPlan {
        schema_version: BLOCK_PLAN_SCHEMA_VERSION,
        source_release_identity: source.release_identity.clone(),
        source_install_generation: source.install_generation,
        target_release_identity: target.release_identity.clone(),
        target_install_generation: target.install_generation,
        files,
    }))
}

/// Blocks of `file` a launcher downloads rather than copies.
pub(crate) fn download_blocks(file: &BlockFilePlan) -> usize {
    file.blocks
        .iter()
        .filter(|block| block.source_offset.is_none())
        .count()
}

/// The first offset of each distinct block of the source file, by block hash.
fn source_block_offsets(path: &Path) -> Result<HashMap<String, u64>, String> {
    let mut offsets = HashMap::new();
    for_each_block(path, |offset, bytes| {
        offsets.entry(block_digest(bytes)).or_insert(offset);
        Ok(())
    })?;
    Ok(offsets)
}

fn target_blocks(
    path: &Path,
    source_offsets: &HashMap<String, u64>,
) -> Result<Vec<BlockDescriptor>, String> {
    let mut blocks = Vec::new();
    for_each_block(path, |_, bytes| {
        let sha256 = block_digest(bytes);
        blocks.push(BlockDescriptor {
            length: u32::try_from(bytes.len())
                .map_err(|_| "a release block exceeds the block size".to_string())?,
            source_offset: source_offsets.get(&sha256).copied(),
            sha256,
        });
        Ok(())
    })?;
    Ok(blocks)
}

fn block_digest(bytes: &[u8]) -> String {
    URL_SAFE_NO_PAD.encode(Sha256::digest(bytes))
}

/// Calls `visit` with each block of the file and its offset; only the last block is short.
fn for_each_block(
    path: &Path,
    mut visit: impl FnMut(u64, &[u8]) -> Result<(), String>,
) -> Result<(), String> {
    let unreadable = || format!("release file {} could not be read", path.display());
    let mut file = File::open(path).map_err(|_| unreadable())?;
    let block_bytes = usize::try_from(BLOCK_PLAN_BLOCK_BYTES)
        .map_err(|_| "the block size does not fit this platform".to_string())?;
    let mut buffer = vec![0_u8; block_bytes];
    let mut offset = 0_u64;
    loop {
        let length = fill(&mut file, &mut buffer).map_err(|_| unreadable())?;
        if length == 0 {
            return Ok(());
        }
        visit(offset, &buffer[..length])?;
        offset += length as u64;
        if length < buffer.len() {
            return Ok(());
        }
    }
}

fn fill(file: &mut File, buffer: &mut [u8]) -> io::Result<usize> {
    let mut filled = 0;
    while filled < buffer.len() {
        match file.read(&mut buffer[filled..]) {
            Ok(0) => break,
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(filled)
}
