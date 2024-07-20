use std::collections::BTreeMap;
use std::fs::File;
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

fn prepare_file(filename: &str) -> File {
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

    file
}

fn write_data(
    file: &mut File,
    data: &BTreeMap<(Txid, u32), UnknownScriptData>,
    ignore_empty: bool,
) {
    for ((txid, vout), data) in data.iter() {
        if ignore_empty && data.output.value == Amount::ZERO {
            continue;
        }

        file.write_all(
            format!(
                "{}; {}; {}:{}; {}; {}\n",
                blk_reader::DateTime::from_timestamp(data.time as i64, 0)
                    .unwrap()
                    .to_string()
                    .replace(" UTC", ""),
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
}

// Usage: cargo run --example list-non-standard-txs -- --max-blocks 1000 --max-files 10 /path/to/blocks
fn main() -> Result<(), std::io::Error> {
    let args = Args::parse();

    println!("Reading blocks: {:?}", args);

    let options = BlockReaderOptions {
        max_blocks: if args.max_blocks == 0 { None } else { Some(args.max_blocks) },
        max_blk_files: if args.max_blk_files == 0 { None } else { Some(args.max_blk_files) },
        max_orphans: if args.max_orphans == 0 { None } else { Some(args.max_orphans) },
        ..Default::default()
    };

    signal_hook::flag::register(signal_hook::consts::SIGTERM, Arc::clone(&options.stop_flag))?;
    signal_hook::flag::register(signal_hook::consts::SIGINT, Arc::clone(&options.stop_flag))?;

    let unspent_filename = "non-standard-unspent.csv";
    let spent_filename = "non-standard-spent.csv";

    let mut unspent_file = prepare_file(unspent_filename);
    let mut spent_file = prepare_file(spent_filename);

    let unspent: BTreeMap<(Txid, u32), UnknownScriptData> = BTreeMap::new();
    let spent: BTreeMap<(Txid, u32), UnknownScriptData> = BTreeMap::new();

    let unspent = std::cell::RefCell::new(unspent);
    let spent = std::cell::RefCell::new(spent);

    let mut reader = BlockReader::new(
        options,
        Box::new(|block, height| {
            let block = block.decode().unwrap();

            let mut unspent = unspent.borrow_mut();

            for tx in block.txdata.iter() {
                for input in tx.input.iter() {
                    let key = (input.previous_output.txid, input.previous_output.vout);

                    // Skip coinbase
                    if input.previous_output.is_null() {
                        continue;
                    }

                    // Remove spent unknown script
                    match unspent.remove(&key) {
                        Some(value) => {
                            spent.borrow_mut().insert(key, value);
                        }
                        None => {}
                    }
                }

                for (vout, output) in tx.output.iter().enumerate() {
                    let script_type = blk_reader::ScriptType::from(&output.script_pubkey);

                    if script_type == ScriptType::Unknown {
                        let key = (tx.compute_txid(), vout as u32);

                        unspent.insert(
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

    println!("Done reading blocks");

    let unspent = unspent.borrow();
    println!("Writing {} items into {}", unspent.len(), unspent_filename);
    write_data(&mut unspent_file, &unspent, args.ignore_empty);

    let spent = spent.borrow();
    println!("Writing {} items into {}", spent.len(), spent_filename);
    write_data(&mut spent_file, &spent, args.ignore_empty);

    Ok(())
}
