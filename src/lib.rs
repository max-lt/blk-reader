mod block;
mod script;

pub use block::BlockReader;
pub use block::BlockReaderOptions;

pub use script::ScriptType;
pub use script::pretty_print_script;

// Re-export chrono types
pub type DateTime = chrono::DateTime<chrono::Utc>;

pub (crate) fn time_str(time: DateTime) -> String {
    time.to_string().replace(" UTC", "")  
}
