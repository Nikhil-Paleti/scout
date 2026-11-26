//! On-disk trigram index for scout.
//!
//! An index is a directory holding `manifest.json` plus immutable segment
//! files (`seg-NNNNNN.idx`) and versioned tombstone bitmaps
//! (`seg-NNNNNN.del-GGGGGG`). Readers load the manifest once and never take
//! locks; writers build new files and swap the manifest with an atomic
//! rename.

pub mod build;
pub mod filter;
pub mod index;
pub mod lock;
pub mod manifest;
pub mod merge;
pub mod rules;
pub mod segment;
pub mod trigram;
pub mod varint;

pub use build::{build_full, BuildOptions, BuildStats};
pub use index::{Index, LiveFile};
pub use manifest::{index_dir_for, Manifest, SegmentInfo};
pub use rules::{Rules, Verdict};
pub use segment::{FileMeta, Segment};
