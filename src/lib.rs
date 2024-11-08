mod chain;
mod block;

pub use block::LazyBlock;
pub use block::BlockReader;
pub use block::BlockReaderOptions;

// Re-export chrono types
pub type DateTime = chrono::DateTime<chrono::Utc>;

pub fn time_str(time: DateTime) -> String {
    time.to_string().replace(" UTC", "")
}
