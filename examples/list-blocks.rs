use std::sync::Arc;

use blk_reader::BlockReader;
use blk_reader::BlockReaderOptions;
use blk_reader::DateTime;

use clap::Parser;

/// Simple program to iterate over all blocks in the blockchain
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Directory containing block files (blk*.dat)
    #[arg(value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
    path: std::path::PathBuf,

    /// Maximum number of blocks to read
    #[arg(long, default_value_t = 653792)]
    max_blocks: u32,

    /// Maximum number of block files to read
    #[arg(long = "max-files", default_value_t = 10_000)]
    max_blk_files: usize,
}

// Usage: cargo run --example list-blocks -- --max-blocks 1000 --max-files 10 /path/to/blocks
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
        ..Default::default()
    };

    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&options.stop_flag))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&options.stop_flag))?;

    let mut reader = BlockReader::new(
        options,
        Box::new(|block, height| {
            println!(
                "Block: {} {:6} {} {} transaction(s)",
                block.block_hash(),
                height,
                DateTime::from_timestamp(block.header.time as i64, 0).unwrap(),
                block.txdata.len()
            );
        }),
    );

    reader.read(&args.path)?;

    Ok(())
}
