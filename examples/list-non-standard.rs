use std::cell::RefCell;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::Write;
use std::sync::Arc;

use bitcoin::block::Header;
use bitcoin::ScriptBuf;
use bitcoin::Amount;
use bitcoin::TxOut;
use bitcoin::Txid;
use blk_reader::BlockReader;
use blk_reader::BlockReaderOptions;

use clap::Parser;

type DateTime = chrono::DateTime<chrono::Utc>;

#[derive(PartialEq)]
pub enum ScriptType {
  P2PK,
  P2PKH,
  P2SH,
  P2WPKH,
  P2WSH,
  P2TR,
  Empty,
  OpReturn,
  Multisig,
  WitnessProgram,
  Unknown,
}

impl From<&ScriptBuf> for ScriptType {
  fn from(script: &ScriptBuf) -> Self {
      if script.is_p2pk() {
          return ScriptType::P2PK;
      }

      if script.is_p2pkh() {
          return ScriptType::P2PKH;
      }

      if script.is_p2sh() {
          return ScriptType::P2SH;
      }

      if script.is_p2wpkh() {
          return ScriptType::P2WPKH;
      }

      if script.is_p2wsh() {
          return ScriptType::P2WSH;
      }

      if script.is_p2tr() {
          return ScriptType::P2TR;
      }

      if script.is_empty() {
          return ScriptType::Empty;
      }

      if script.is_op_return() {
          return ScriptType::OpReturn;
      }

      if script.is_multisig() {
          return ScriptType::Multisig;
      }

      if script.is_witness_program() {
          return ScriptType::WitnessProgram;
      }

      ScriptType::Unknown
  }
}

// https://github.com/bitcoin/bitcoin/blob/master/src/addresstype.cpp#L49
impl ToString for ScriptType {
  fn to_string(&self) -> String {
      match self {
          ScriptType::P2PK => "P2PK".to_string(),
          ScriptType::P2PKH => "P2PKH".to_string(),
          ScriptType::P2SH => "P2SH".to_string(),
          ScriptType::P2WPKH => "P2WPKH".to_string(),
          ScriptType::P2WSH => "P2WSH".to_string(),
          ScriptType::P2TR => "P2TR".to_string(),
          ScriptType::Empty => "Empty".to_string(),
          ScriptType::OpReturn => "OpReturn".to_string(),
          ScriptType::Multisig => "MultiSig".to_string(),
          ScriptType::WitnessProgram => "WitnessProgram".to_string(),
          ScriptType::Unknown => "UNKNOWN".to_string(),
      }
  }
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
                DateTime::from_timestamp(data.time as i64, 0)
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

    let unspent: BTreeMap<(Txid, u32), UnknownScriptData> = BTreeMap::new();
    let spent: BTreeMap<(Txid, u32), UnknownScriptData> = BTreeMap::new();

    let unspent = std::cell::RefCell::new(unspent);
    let spent = std::cell::RefCell::new(spent);

    let last_block_height: RefCell<u32> = RefCell::new(0);
    let last_block_header: RefCell<Option<Header>> = RefCell::new(None);

    let mut reader = BlockReader::new(options);

    reader.set_block_cb(
        Box::new(|block, height| {
            last_block_header.replace(Some(block.header));
            last_block_height.replace(height);

            let block = block.decode().unwrap();

            let mut unspent = unspent.borrow_mut();

            for tx in block.txdata.iter() {
                let mut txid: Option<Txid> = None; // Compute txid only if needed

                for input in tx.input.iter() {
                    let key = (input.previous_output.txid, input.previous_output.vout);

                    // Skip coinbase
                    if input.previous_output.is_null() {
                        continue;
                    }

                    // Remove from unspent and add to spent
                    match unspent.remove(&key) {
                        Some(value) => {
                            spent.borrow_mut().insert(key, value);
                        }
                        None => {}
                    }
                }

                for (vout, output) in tx.output.iter().enumerate() {
                    let script_type = ScriptType::from(&output.script_pubkey);

                    if script_type == ScriptType::Unknown {
                        let txid = match txid {
                            Some(txid) => txid,
                            None => {
                                let computed = tx.compute_txid();
                                txid = Some(computed.clone());
                                computed
                            },
                        };

                        let key = (txid, vout as u32);

                        unspent.insert(
                            key,
                            UnknownScriptData {
                                time: block.header.time,
                                height,
                                output: output.clone(),
                            },
                        );
                    }
                }
            }
        })
    );

    reader.read(&args.path)?;

    let last_block_height = last_block_height.take();
    let last_block_id = last_block_header.take().unwrap();
    println!("Done reading blocks. Last block is {} {}", last_block_height, last_block_id.block_hash());

    let spent = spent.borrow();
    let unspent = unspent.borrow();

    let unspent_filename = "non-standard-unspent.csv";
    let mut unspent_file = prepare_file(unspent_filename);
    println!("Writing {} items into {}", unspent.len(), unspent_filename);
    write_data(&mut unspent_file, &unspent, false);

    let unspent_filename = "non-standard-unspent-non-zero.csv";
    let mut unspent_file = prepare_file(unspent_filename);
    println!("Writing {} items into {}", unspent.len(), unspent_filename);
    write_data(&mut unspent_file, &unspent, true);

    let spent_filename = "non-standard-spent.csv";
    let mut spent_file = prepare_file(spent_filename);
    println!("Writing {} items into {}", spent.len(), spent_filename);
    write_data(&mut spent_file, &spent, false);

    let spent_filename = "non-standard-spent-non-zero.csv";
    let mut spent_file = prepare_file(spent_filename);
    println!("Writing {} items into {}", spent.len(), spent_filename);
    write_data(&mut spent_file, &spent, true);

    Ok(())
}
