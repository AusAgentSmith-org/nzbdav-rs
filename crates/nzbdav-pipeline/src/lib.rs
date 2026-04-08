//! NZB processing pipeline — deobfuscation, file processors, aggregators.
//!
//! Pipeline stages:
//! 1. Deobfuscation (fetch first segments, PAR2 descriptors, file info matching)
//! 2. File processing (RAR header reading, plain file validation)
//! 3. Aggregation (group RAR volumes, create DavMultipartFile entries)
//! 4. Post-processing (rename duplicates, blocklist, STRM files)

pub mod aggregators;
pub mod deobfuscation;
pub mod error;
pub mod processors;
pub mod types;
pub mod post_processors;
pub mod queue_item_processor;
