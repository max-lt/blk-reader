use blk_reader::BlockReader;
use blk_reader::BlockReaderOptions;
use blk_reader::ScriptType;

use clap::Parser;

/// Simple program to iterate over all blocks in the blockchain
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    /// Directory containing block files (blk*.dat)
    #[arg(value_name = "DIR", value_hint = clap::ValueHint::DirPath)]
    path: std::path::PathBuf,

    /// Maximum number of blocks to read
    #[arg(long, default_value_t = 1_000)]
    max_blocks: u32,

    /// Maximum number of block files to read
    #[arg(long = "max-files", default_value_t = 10_000)]
    max_blk_files: usize,
}

// Usage: cargo run --example list-non-standard-txs -- --max-blocks 1000 --max-files 10 /path/to/blocks
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
        Box::new(|block, height| {
            for tx in block.txdata.iter() {
                for output in tx.output.iter() {
                    let script_type = blk_reader::ScriptType::from(&output.script_pubkey);

                    if script_type == ScriptType::Unknown {
                        println!(
                            "In block {}, transaction {}, invalid output ({}): \n {}",
                            height,
                            tx.compute_txid(),
                            output.value,
                            output.script_pubkey.to_string(),
                        );
                    }
                }
            }
        }),
    );

    reader.read(&args.path)?;

    Ok(())
}
