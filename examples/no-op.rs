use blk_reader::BlockReader;
use blk_reader::BlockReaderOptions;

use clap::Parser;

/// Simple program to iterate over all blocks in the blockchain
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Directory containing block files (blk*.dat)
    #[arg(value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
    path: std::path::PathBuf,

    /// Maximum number of blocks to read
    #[arg(long, default_value_t = 1000)]
    max_blocks: u32,

    /// Maximum number of block files to read
    #[arg(long = "max-files", default_value_t = 10_000)]
    max_blk_files: usize,
}

// Usage: cargo run --example no-op -- --max-blocks 1000 --max-files 10 /path/to/blocks
fn main() -> Result<(), std::io::Error> {
    let args = Args::parse();

    println!(
        "Reading blocks from: {} (max blocks: {}, max blk files: {})",
        args.path.to_string_lossy(),
        args.max_blocks,
        args.max_blk_files
    );

    let options = BlockReaderOptions {
        max_blocks: args.max_blocks,
        max_blk_files: args.max_blk_files,
    };

    let mut reader = BlockReader::new(
        options,
        Box::new(|_block, _height| {
            // Do nothing to evaluate time to read (and order) blocks
            // without any processing  
        }),
    );

    reader.read(&args.path)?;

    Ok(())
}
