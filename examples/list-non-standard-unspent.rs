use std::collections::BTreeMap;
use std::io::Write;

// use bitcoin::ScriptBuf;
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

    /// Maximum number of blocks to read
    #[arg(long, default_value_t = 653792)]
    max_blocks: u32,

    /// Maximum number of block files to read
    #[arg(long = "max-files", default_value_t = 10_000)]
    max_blk_files: usize,
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
        max_blocks: args.max_blocks,
        max_blk_files: args.max_blk_files,
    };

    let filename = "non-standard-unspent-txs.csv";

    // Delete file if it exists
    std::fs::remove_file(filename).unwrap_or_default();

    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(filename)
        .unwrap();

    // Headers
    file.write_all(format!("sep=;\n\"Block Time\"; Block; Tx:Vout; Value; Script\n").as_bytes())
        .unwrap();

    let unknown: BTreeMap<(Txid, u32), UnknownScriptData> = BTreeMap::new();

    let unknown = std::cell::RefCell::new(unknown);

    let mut reader = BlockReader::new(
        options,
        Box::new(|block, height| {
            let mut unknown = unknown.borrow_mut();

            for tx in block.txdata.iter() {
                for input in tx.input.iter() {
                    let key = (input.previous_output.txid, input.previous_output.vout);

                    // Skip coinbase
                    if input.previous_output.is_null() {
                        continue;
                    }

                    // Remove spent unknown script
                    match unknown.remove(&key) {
                        Some(_) => {}
                        None => {}
                    }
                }

                for (vout, output) in tx.output.iter().enumerate() {
                    let script_type = blk_reader::ScriptType::from(&output.script_pubkey);

                    if script_type == ScriptType::Unknown {
                        let key = (tx.compute_txid(), vout as u32);

                        unknown.insert(
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

    println!("Done reading blocks, writing non-standard-non-empty-txs.csv");

    for ((txid, vout), data) in unknown.borrow().iter() {
        file.write_all(
            format!(
                "{}; {}; {}:{}; {}; \"{}\"\n",
                blk_reader::DateTime::from_timestamp(data.time as i64, 0).unwrap(),
                data.height - 1,
                txid,
                vout,
                data.output.value,
                data.output.script_pubkey.to_string()
            )
            .as_bytes(),
        )
        .unwrap();
    }

    Ok(())
}
