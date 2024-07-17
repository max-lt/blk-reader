use std::fs;
use std::fs::File;
use std::io::BufReader;
use std::io::Error;
use std::io::ErrorKind;
use std::io::Read;

use std::collections::BTreeMap;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;

use bitcoin::consensus::Decodable;
use bitcoin::hashes::Hash;
use bitcoin::p2p::Magic;
use bitcoin::Block;
use bitcoin::BlockHash;

static MAGIC: Magic = Magic::BITCOIN;

use chrono::DateTime;
use chrono::Utc;

use crate::time_str;

fn block_time(time: i64) -> DateTime<Utc> {
    DateTime::from_timestamp(time, 0).unwrap()
}

fn system_time() -> chrono::DateTime<Utc> {
    let time = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();

    DateTime::from_timestamp(time as i64, 0).unwrap()
}

pub struct BlockReader<'call> {
    height: u32,
    last_block_hash: BlockHash,
    orphans: BTreeMap<BlockHash, Block>,
    block_cb: Box<dyn Fn(Block, u32) + 'call>,
    options: BlockReaderOptions,
}

pub struct BlockReaderOptions {
    pub max_blocks: Option<u32>,
    pub max_orphans: Option<usize>,
    pub max_blk_files: Option<usize>,
    pub stop_flag: std::sync::Arc<std::sync::atomic::AtomicBool>,
}

impl Default for BlockReaderOptions {
    fn default() -> Self {
        BlockReaderOptions {
            max_blocks: Some(1_000),
            max_orphans: Some(10_000),
            max_blk_files: None,
            stop_flag: Arc::new(AtomicBool::new(false)),
        }
    }
}

impl<'a> BlockReader<'a> {
    pub fn new(
        options: BlockReaderOptions,
        block_cb: Box<dyn Fn(Block, u32) + 'a>,
    ) -> BlockReader<'a> {
        BlockReader {
            height: 0,
            last_block_hash: BlockHash::all_zeros(),
            orphans: BTreeMap::new(),
            block_cb: block_cb,
            options: options,
        }
    }

    /// Read the directory and return a list of files
    fn read_dir(&self, dir_path: &std::path::Path) -> Result<Vec<String>, Error> {
        let mut entries: Vec<String> = fs::read_dir(dir_path)?
            .filter_map(Result::ok)
            .map(|d| d.path())
            .filter(|d| d.is_file() && d.extension().is_some())
            .map(|d| d.to_str().unwrap().to_string())
            .filter(|s| s.contains("/blk") && s.ends_with(".dat"))
            .collect();

        entries.sort();

        match self.options.max_blk_files {
            Some(max_blk_files) => entries.truncate(max_blk_files),
            None => (),
        }

        return Ok(entries);
    }

    /// Read blocks from a file and insert them into the index
    /// Return true if there are more blocks to read, false if we reached the end of the file
    fn read_blocs(&mut self, file_path: &str) -> Result<bool, Error> {
        println!("{} read {}", time_str(system_time()), file_path);

        let file = File::open(file_path)?;
        let file_size = file.metadata().unwrap().len();

        let mut offset = 0; // Buffer offset

        let mut reader = BufReader::new(file);

        loop {
            let magic = Magic::consensus_decode(&mut reader).unwrap();
            let size = u32::consensus_decode(&mut reader).unwrap() as usize;

            if magic != MAGIC {
                println!("Magic is not correct");
                return Err(Error::new(ErrorKind::Other, "Magic is not correct"));
            }

            // Limit reader to the block size
            let mut data: Vec<u8> = vec![0; size];
            reader.read_exact(&mut data).unwrap();
            let mut block_reader: &[u8] = &data[..];

            offset += 4 + 4 + size as u64;

            let block = Block::consensus_decode(&mut block_reader).unwrap();
            let time = block.header.time as i64;
            self.insert(block);

            // Stop signal received
            if self
                .options
                .stop_flag
                .load(std::sync::atomic::Ordering::Relaxed)
            {
                println!("Stop signal received");
                return Ok(false);
            }

            // We reached the limit of blocks, stop here
            if self.max_height_reached() {
                println!("Reached limit of blocks {}", self.height);
                return Ok(false);
            }

            // We reached the limit of orphan blocks, stop here
            if self.max_orphans_reached() {
                println!("Reached limit of orphan blocks {}", self.orphans.len());
                return Ok(false);
            }

            // End of file, there are more blocks to read in the next file
            if offset >= file_size {
                println!(
                    "{} done {} {} {} orphans={}",
                    time_str(system_time()),
                    file_path,
                    self.height,
                    time_str(block_time(time)),
                    self.orphans()
                );
                return Ok(true);
            }
        }
    }

    /// Insert a block into the index
    fn insert(&mut self, block: Block) {
        // This new block not the next block in the chain, add it to the orphans
        if self.last_block_hash != block.header.prev_blockhash {
            self.orphans.insert(block.header.prev_blockhash, block);
            return;
        }

        // This new block is now the tail of the chain
        self.push_block(block);
        if self.max_height_reached() {
            return;
        }

        loop {
            let block = match self.orphans.remove(&self.last_block_hash) {
                Some(block) => block,
                None => break,
            };

            self.push_block(block);
            if self.max_height_reached() {
                return;
            }
        }
    }

    fn push_block(&mut self, block: Block) {
        self.height += 1;
        self.last_block_hash = block.block_hash();

        // Call the callback function
        (self.block_cb)(block, self.height);
    }

    pub fn read(&mut self, dir_path: &std::path::Path) -> Result<(), Error> {
        let entries = BlockReader::read_dir(&self, dir_path)?;

        for entry in entries {
            if self.max_height_reached() {
                break;
            }

            if !self.read_blocs(&entry)? {
                break;
            }
        }

        Ok(())
    }

    /// Return the number of orphans blocks
    pub fn orphans(&self) -> usize {
        self.orphans.len()
    }

    /// Return the height of the last block
    pub fn height(&self) -> u32 {
        self.height
    }

    fn max_height_reached(&self) -> bool {
        match self.options.max_blocks {
            Some(max_blocks) => self.height >= max_blocks,
            None => false            
        }
    }

    fn max_orphans_reached(&self) -> bool {
        match self.options.max_orphans {
            Some(max_orphans) => self.orphans.len() >= max_orphans,
            None => false
        }
    }
}
