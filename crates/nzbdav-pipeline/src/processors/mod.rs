pub mod file_processor;
pub mod rar_processor;

pub use file_processor::process_plain_files;
pub use rar_processor::process_rar_files;
