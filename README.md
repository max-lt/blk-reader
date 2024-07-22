# blk-reader
Efficiently read and extract data from Bitcoin Core blk files.

## Pre-requisites

In order to run the examples, you need to have the blk files from the Bitcoin Core data directory and edit the permissions to allow the current user to read the files.

```bash
sudo chmod 664 /path/to/blk/dir/blk*
```

## Examples

To build examples, run:

```bash
cargo build --examples --release
```

To run examples, use the following commands:
```bash
./target/release/examples/<example-name> /path/to/blk/dir <args>

or 

cargo run --example <example-name> /path/to/blk/dir <args>
```

### list-blocks

Usage: `list-blocks <blk-dir> [--max-blocks <max-blocks>] [--max-files <max-block-files>]`

```bash
list-blocks /path/to/blk/dir --max-blocks 10
```

### list-non-standard

```bash
list-non-standard /path/to/blk/dir --max-blocks 100000
```

Note: **Block are indexed from 0**, so if you want to read up to block 100000 inclusive, you need to pass `--max-blocks 100001`.
