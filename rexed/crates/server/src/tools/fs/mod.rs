//! Filesystem tool facade. Reading, discovery and identity have separate owners.
mod glob;
mod read;
mod stamp;
mod stat;
#[cfg(test)]
mod test;

pub use glob::{glob_paths, GlobOptions};
pub use read::{read_path, ReadMode, ReadOptions};
pub use stamp::{file_hash_code, file_stamp, hash_bytes, FileKind, FileStamp};
pub use stat::{stat_path, StatOptions};
