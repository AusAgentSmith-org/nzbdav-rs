pub mod file_aggregator;
pub mod rar_aggregator;

pub use file_aggregator::aggregate_plain_files;
pub use rar_aggregator::aggregate_rar_files;
