use std::sync::Arc;

use blk_reader::BlockReader;
use blk_reader::BlockReaderOptions;

use clap::Parser;

type DateTime = chrono::DateTime<chrono::Utc>;

fn time_str(time: DateTime) -> String {
    time.to_string().replace(" UTC", "")
}

fn block_time(time: u32) -> DateTime {
    DateTime::from_timestamp(time as i64, 0).unwrap()
}

/// Simple program to iterate over all blocks in the blockchain
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Directory containing block files (blk*.dat)
    #[arg(value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
    path: std::path::PathBuf,

    /// Maximum number of orphans to keep in memory
    #[arg(long, default_value_t = 10_000)]
    max_orphans: usize,

    /// Maximum number of blocks to read
    #[arg(long, default_value_t = 850_150)]
    max_blocks: u32,

    /// Maximum number of block files to read
    #[arg(long = "max-files", default_value_t = 0)]
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
        max_blocks: if args.max_blocks == 0 {
            None
        } else {
            Some(args.max_blocks)
        },
        max_blk_files: if args.max_blk_files == 0 {
            None
        } else {
            Some(args.max_blk_files)
        },
        max_orphans: if args.max_orphans == 0 {
            None
        } else {
            Some(args.max_orphans)
        },
        ..Default::default()
    };

    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&options.stop_flag))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&options.stop_flag))?;

    let mut reader = BlockReader::new(options);

    reader.set_block_cb(Box::new(|block, height| {
        let offset = &block.offset;
        let blk_path = &block.blk_path;
        let blk_index = &block.blk_index;
        let block = block.decode().unwrap();

        println!(
            "Block: {} {} {} in {} {} (offset={}) {} transaction(s)",
            block.block_hash(),
            height,
            DateTime::from_timestamp(block.header.time as i64, 0).unwrap(),
            blk_path,
            blk_index,
            offset,
            block.txdata.len()
        );
    }));

    reader.set_file_cb(Box::new(|file, height, time| {
        println!(
            "done reading {} {} {}",
            file.get(file.len() - 12..).unwrap_or(file.as_str()),
            height,
            time_str(block_time(time))
        );
    }));

    reader.read(&args.path)?;

    Ok(())
}
