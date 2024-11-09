use std::fs;
use std::fs::File;
use std::io::BufReader;
use std::io::Error;
use std::io::ErrorKind;
use std::io::Read;

use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::vec;

use bitcoin::block::Header;
use bitcoin::consensus::Decodable;
use bitcoin::hashes::Hash;
use bitcoin::p2p::Magic;
use bitcoin::Block;
use bitcoin::BlockHash;
use bitcoin::Transaction;

static MAGIC: Magic = Magic::BITCOIN;

use crate::chain::Chain;
use crate::chain::GetBlockIds;

#[derive(Debug, Clone)]
pub struct LazyBlock {
    pub blk_index: u32,
    pub blk_path: String,
    pub offset: u64,
    pub header: Header,
    data: Vec<u8>,
}

impl LazyBlock {
    pub fn decode(&self) -> Result<Block, bitcoin::consensus::encode::Error> {
        let mut txdata: &[u8] = &self.data[..];
        let txdata = Vec::<Transaction>::consensus_decode(&mut txdata)?;
        Ok(Block {
            header: self.header,
            txdata,
        })
    }
}

impl GetBlockIds<BlockHash> for LazyBlock {
    fn get_block_id(&self) -> BlockHash {
        self.header.block_hash()
    }

    fn get_block_prev_id(&self) -> BlockHash {
        self.header.prev_blockhash
    }
}

pub struct BlockReader<'call> {
    height: u32,
    chain: Chain<BlockHash, LazyBlock>,
    block_cb: Option<Box<dyn Fn(LazyBlock, u32) + 'call>>,
    file_cb: Option<Box<dyn Fn(String, u32, u32) + 'call>>,
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
    pub fn new(options: BlockReaderOptions) -> BlockReader<'a> {
        BlockReader {
            height: 0,
            chain: Chain::new(BlockHash::all_zeros()),
            block_cb: None,
            file_cb: None,
            options,
        }
    }

    pub fn set_block_cb(&mut self, block_cb: Box<dyn Fn(LazyBlock, u32) + 'a>) {
        self.block_cb = Some(block_cb);
    }

    pub fn set_file_cb(&mut self, file_cb: Box<dyn Fn(String, u32, u32) + 'a>) {
        self.file_cb = Some(file_cb);
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
        let file = File::open(file_path)?;
        let file_size = file.metadata().unwrap().len();

        let file_path_len = file_path.len();
        let blk_index = file_path[file_path_len - 9..file_path_len - 4]
            .parse::<u32>()
            .unwrap();

        let mut offset = 0; // Buffer offset

        let mut reader = BufReader::new(file);

        loop {
            let magic = Magic::consensus_decode(&mut reader).unwrap();
            if magic != MAGIC {
                println!(
                    "Magic is not correct in {} offset={}; got {}",
                    file_path, offset, magic
                );
                return Err(Error::new(ErrorKind::Other, "Magic is not correct"));
            }

            let size = u32::consensus_decode(&mut reader).unwrap() as usize;

            // Read the block header
            let header = Header::consensus_decode(&mut reader).unwrap();

            let time = header.time;
            let height: u32 = self.height;

            // Skip the rest of the block
            let mut data = vec![0; size - 80];
            reader.read_exact(&mut data).unwrap();

            // Insert the block into the index
            self.insert(LazyBlock {
                header,
                data,
                offset,
                blk_path: file_path.to_string(),
                blk_index,
            });

            offset += 4 + 4 + size as u64;

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
                println!(
                    "Reached limit of blocks. Next block is {} {}",
                    height,
                    self.chain.next_id()
                );
                return Ok(false);
            }

            // We reached the limit of orphan blocks, stop here
            if self.max_orphans_reached() {
                println!("Reached limit of orphan blocks {}", self.orphans());
                return Ok(false);
            }

            // End of file, there are more blocks to read in the next file
            if offset >= file_size {
                if let Some(ref file_cb) = self.file_cb {
                    file_cb(file_path.to_string(), height, time);
                }

                return Ok(true);
            }
        }
    }

    /// Insert a block into the index
    fn insert(&mut self, block: LazyBlock) {
        self.chain.insert(block);

        while self.chain.longest_chain_depth() >= 10 {
            match self.chain.pop_head() {
                Some(block) => {
                    self.push_block(block);
                    if self.max_height_reached() {
                        return;
                    }
                }
                None => return,
            }
        }
    }

    fn push_block(&mut self, block: LazyBlock) {
        let height = self.height;

        self.height += 1;

        // Call the callback function
        if let Some(ref block_cb) = self.block_cb {
            block_cb(block, height);
        }
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
        self.chain.orphans()
    }

    /// Return the height of the last block
    pub fn height(&self) -> u32 {
        self.height
    }

    fn max_height_reached(&self) -> bool {
        match self.options.max_blocks {
            Some(max_blocks) => self.height >= max_blocks,
            None => false,
        }
    }

    fn max_orphans_reached(&self) -> bool {
        match self.options.max_orphans {
            Some(max_orphans) => self.orphans() >= max_orphans,
            None => false,
        }
    }
}
