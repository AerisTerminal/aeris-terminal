//! Catalog chunking and reassembly.

use prost::Message as _;

use crate::error::ProtocolError;
use crate::messages::{CatalogEntry, CatalogSnapshot};

/// Conservative per-chunk encoded budget (512 KiB) keeping catalog frames well
/// under [`crate::MAX_FRAME_BYTES`].
pub const CATALOG_CHUNK_BUDGET_BYTES: usize = 512 * 1024;

/// Headroom reserved for the fixed-size snapshot fields and framing prefix.
const SNAPSHOT_OVERHEAD_BYTES: usize = 64;

fn entry_framed_len(entry: &CatalogEntry) -> usize {
    let encoded = entry.encoded_len();
    1 + prost::length_delimiter_len(encoded) + encoded
}

/// Splits catalog entries into chunks whose encoded size stays under
/// [`CATALOG_CHUNK_BUDGET_BYTES`]. An empty catalog yields one empty chunk.
#[must_use]
pub fn split_catalog(entries: Vec<CatalogEntry>, revision: u64) -> Vec<CatalogSnapshot> {
    let budget = CATALOG_CHUNK_BUDGET_BYTES.saturating_sub(SNAPSHOT_OVERHEAD_BYTES);
    let mut chunks: Vec<Vec<CatalogEntry>> = Vec::new();
    let mut current: Vec<CatalogEntry> = Vec::new();
    let mut current_len = 0_usize;
    for entry in entries {
        let entry_len = entry_framed_len(&entry);
        if !current.is_empty() && current_len.saturating_add(entry_len) > budget {
            chunks.push(std::mem::take(&mut current));
            current_len = 0;
        }
        current_len = current_len.saturating_add(entry_len);
        current.push(entry);
    }
    if !current.is_empty() || chunks.is_empty() {
        chunks.push(current);
    }

    let chunk_count = u32::try_from(chunks.len()).unwrap_or(u32::MAX);
    chunks
        .into_iter()
        .enumerate()
        .map(|(index, entries)| CatalogSnapshot {
            revision,
            chunk_index: u32::try_from(index).unwrap_or(u32::MAX),
            chunk_count,
            entries,
        })
        .collect()
}

/// Reassembles catalog chunks for one revision into the full entry list.
///
/// Chunks may arrive in any order. A chunk for a different revision while a
/// transfer is in flight is rejected; once a transfer completes the assembler
/// is ready for the next revision.
#[derive(Clone, Debug, Default)]
pub struct CatalogReassembler {
    revision: Option<u64>,
    chunk_count: u32,
    received: u32,
    chunks: Vec<Option<Vec<CatalogEntry>>>,
}

impl CatalogReassembler {
    /// Creates an idle reassembler.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one chunk, returning the full entry list once every chunk of the
    /// revision has arrived.
    ///
    /// # Errors
    ///
    /// Returns [`ProtocolError::CatalogRevisionConflict`] when a chunk belongs
    /// to a different revision than the transfer in flight, or
    /// [`ProtocolError::CatalogChunkOutOfRange`] when the chunk index exceeds
    /// the declared chunk count.
    pub fn push(
        &mut self,
        snapshot: CatalogSnapshot,
    ) -> Result<Option<Vec<CatalogEntry>>, ProtocolError> {
        if let Some(expected) = self.revision {
            if expected != snapshot.revision {
                return Err(ProtocolError::CatalogRevisionConflict {
                    expected,
                    found: snapshot.revision,
                });
            }
        } else {
            self.revision = Some(snapshot.revision);
            self.chunk_count = snapshot.chunk_count;
            self.received = 0;
            self.chunks = (0..snapshot.chunk_count).map(|_| None).collect();
        }

        if snapshot.chunk_count != self.chunk_count {
            return Err(ProtocolError::CatalogChunkOutOfRange {
                index: snapshot.chunk_index,
                count: snapshot.chunk_count,
            });
        }
        let index = usize::try_from(snapshot.chunk_index).map_err(|_| {
            ProtocolError::CatalogChunkOutOfRange {
                index: snapshot.chunk_index,
                count: self.chunk_count,
            }
        })?;
        let slot = self.chunks.get_mut(index).ok_or({
            ProtocolError::CatalogChunkOutOfRange {
                index: snapshot.chunk_index,
                count: self.chunk_count,
            }
        })?;
        if slot.is_none() {
            self.received = self.received.saturating_add(1);
            *slot = Some(snapshot.entries);
        }

        if self.received < self.chunk_count {
            return Ok(None);
        }
        let entries = self
            .chunks
            .iter_mut()
            .filter_map(Option::take)
            .flatten()
            .collect();
        self.reset();
        Ok(Some(entries))
    }

    /// Drops any partially assembled transfer.
    pub fn reset(&mut self) {
        self.revision = None;
        self.chunk_count = 0;
        self.received = 0;
        self.chunks.clear();
    }
}
