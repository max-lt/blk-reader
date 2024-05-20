# blk-reader
Efficiently read and extract data from Bitcoin Core blk files.

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

Usage: `list-blocks <blk-dir> [--max-blocks <max-blocks>] [--max-files <start-block>]`

```bash
list-blocks /path/to/blk/dir --max-blocks 10
```

### list-non-standard-txs

```bash
list-non-standard-txs /path/to/blk/dir --max-blocks 100000
```
