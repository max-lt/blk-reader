use std::collections::BTreeMap;
use std::io::Write;
use std::sync::Arc;

use bitcoin::Amount;
use bitcoin::TxOut;
use bitcoin::Txid;
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

    /// Maximum number of orphans to keep in memory
    #[arg(long, default_value_t = 10_000)]
    max_orphans: usize,

    /// Maximum number of blocks to read
    #[arg(long, default_value_t = 850_150)]
    max_blocks: u32,

    /// Maximum number of block files to read
    #[arg(long = "max-files", default_value_t = 0)]
    max_blk_files: usize,

    /// Ignore empty outputs
    #[arg(long = "ignore-empty", default_value_t = false)]
    ignore_empty: bool,
}

struct UnknownScriptData {
    time: u32,
    height: u32,
    output: TxOut,
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
        max_blocks: if args.max_blocks == 0 { None } else { Some(args.max_blocks) },
        max_blk_files: if args.max_blk_files == 0 { None } else { Some(args.max_blk_files) },
        max_orphans: if args.max_orphans == 0 { None } else { Some(args.max_orphans) },
        ..Default::default()
    };

    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&options.stop_flag))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&options.stop_flag))?;

    let filename = "non-standard-txs.csv";

    // Delete file if it exists
    std::fs::remove_file(filename).unwrap_or_default();

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(filename)
        .unwrap();

    // Headers
    file.write_all(format!("sep=;\n\"Block Time\"; Block; Tx; Value; Script\n").as_bytes())
        .unwrap();

    let non_standard: BTreeMap<(Txid, u32), UnknownScriptData> = BTreeMap::new();
    let non_standard = std::cell::RefCell::new(non_standard);

    let mut reader = BlockReader::new(
        options,
        Box::new(|block, height| {
            let block = block.decode().unwrap();

            for tx in block.txdata.iter() {
                for (vout, output) in tx.output.iter().enumerate() {
                    let script_type = blk_reader::ScriptType::from(&output.script_pubkey);

                    if script_type == ScriptType::Unknown {
                        let key = (tx.compute_txid(), vout as u32);
                        non_standard.borrow_mut().insert(
                            key,
                            UnknownScriptData {
                                time: block.header.time,
                                height: height - 1,
                                output: output.clone(),
                            },
                        );
                    }
                }
            }
        }),
    );

    reader.read(&args.path)?;

    let non_standard = non_standard.borrow();
    
    println!("Done reading blocks, writing {} outputs to {}", non_standard.len(), filename);

    for ((txid, vout), data) in non_standard.iter() {
        if args.ignore_empty && data.output.value == Amount::ZERO {
            continue;
        }

        file.write_all(
            format!(
                "{}; {}; {}:{}; {}; {}\n",
                blk_reader::DateTime::from_timestamp(data.time as i64, 0).unwrap().to_string().replace(" UTC", ""),
                data.height,
                txid,
                vout,
                data.output.value.to_btc(),
                data.output.script_pubkey.to_string()
            )
            .as_bytes(),
        )
        .unwrap();
    }

    Ok(())
}
