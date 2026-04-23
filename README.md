# el-reward-calculator
Execution Layer reward calculator

## **Overview**

This Rust project parses and processes validators mev-boost,commit-boost & vouch logs . It extracts key information such as slot details, bids, block hashes, relay URLs, and calculates reward increases, including payload delivery and block hashes. It supports multiple validator clients  and stores the data either in JSON or CSV format.

## **Key Features**
- **Log Parsing**: Reads and processes logs for different validator side car clients.
- **Data Extraction**: Extracts details such as slot UID, block number, bid values, relay URLs, and timestamps.
- **Reward Calculation**: Calculates reward increases (in ether) based on the highest and second-highest bids.
- **Output Formats**: Outputs the processed data in either **JSON** or **CSV** format.

## **Dependencies**

- `serde` - for serializing and deserializing data.
- `serde_json` - for handling JSON data.
- `chrono` - for date and time handling.
- `regex` - for pattern matching and extracting specific information from logs.
- `csv` - for writing data to CSV files.
- `ethers` - for handling Ethereum-related types and utilities.
- `url` - for parsing and handling URLs.

## **Setup**

### **1. Install Rust**

Ensure that you have **Rust** installed. If not, you can install it by following the instructions from the official Rust website: [Install Rust](https://www.rust-lang.org/tools/install).

### **2. Dependencies**

The dependencies are automatically handled by **Cargo** (Rust's package manager). To install them, simply clone this repository and run:

```bash
cargo build
```

### **3.Running the Application**

The main entry point for the program is the `main` function in `src/main.rs`. To run the program, use the following command:

```bash
cargo run -- <log_file_path> <validator_client_id_log_format> [output_format]
```
- log_file_path: Path to the log file you want to process.
- validator_client_id log format: validators side car client id log format (either mevboost_text,mevboost_json, commitboost_text,commitboost_json, vouch).
- output_format (Optional): The format to output the processed data. It can be either json or csv (default is json).

#### **3.1 Example Usage**
To process text logs and save the output in CSV format:
```bash
cargo run -- path/to/log_file.txt mevbosst_text csv
```
To process json logs and output in JSON:
```bash
cargo run -- path/to/log_file.json commitboost_json json
```
To process json logs and output in JSON for vouch:
```bash
cargo run path/to/log_file.json vouch csv
```
#### **3.2 Output file path**
- The output files are stored in a directory structure based on the current date.
he program automatically creates a folder named using the date in the format DD_MM_YYYY, and then stores the output files within that folder.
- **Directory Structure**: The files will be stored in a directory named
slot_infos/{DD_MM_YYYY}/, where {DD_MM_YYYY} is the current date (e.g., 17_03_2025).
- **CSV Output**: A CSV file will be generated in the format:
```bash
slot_infos_{DD_MM_YYYY}_{HH_MM_SS}.csv
```
- **JSON Output**: The processed data will be saved in a JSON file with the following structure:
```bash
slot_infos_{DD_MM_YYYY}_{HH_MM_SS}.json
```

#### **3.2 Output format**
- **CSV Output**: A CSV file will be generated in the format:
```bash
slot_uid,slot,block_number,header_start_ms_into_slot,payload_start_ms_into_slot,block_hash,is_proxy_win,is_winning_bid_highest,...
```
- **JSON Output**: The processed data will be saved in a JSON file with the following structure:
```bash
{
  "11035091": {
    "1823c10b-9fab-4960-8ca6-2bd999d83432": {
      "slot_uid": "1823c10b-9fab-4960-8ca6-2bd999d83432",
      "slot": "11035091",
      "block_number": "21820646",
      "info": {
        "header_start_ms_into_slot": 0,
        "bids": [
          {
            "timestamp": 1739245115,
            "pubkey": "0xb66941166ddf594deca81f707c0f8d60a5d62e222d52b54dbbee6079596d29e23afa43adac7f49bc064dac50af8d8e00",
            "block_hash": "0xaa7a2f7c8ca66850d9e716d4dfc5b12cab2dbff8d09161af9dec9ebaef21929b",
            "parent_hash": "0xfc4140c6a665bfebdd841930a2b2abc728b981a3d1b61af0c0aa39ffcd0d50d6",
            "block_number": "21820646",
            "slot": "11035091",
            "ua": "Lighthouse/v6.0.1-0d90135",
            "relay": "https://censoring.titanrelay.xyz/eth/v1/builder/header/11035091/0xfc4140c6a665bfebdd841930a2b2abc728b981a3d1b61af0c0aa39ffcd0d50d6/0xb66941166ddf594deca81f707c0f8d60a5d62e222d52b54dbbee6079596d29e23afa43adac7f49bc064dac50af8d8e00",
            "bid_value": 0.00993944286898311
          },
          {
            "timestamp": 1739245115,
            "pubkey": "0xb66941166ddf594deca81f707c0f8d60a5d62e222d52b54dbbee6079596d29e23afa43adac7f49bc064dac50af8d8e00",
            "block_hash": "0x6322453128d193eba3c117838bac2f33c8100e7e5a69f6001c2f8eded5be3247",
            "parent_hash": "0xfc4140c6a665bfebdd841930a2b2abc728b981a3d1b61af0c0aa39ffcd0d50d6",
            "block_number": "21820646",
            "slot": "11035091",
            "ua": "Lighthouse/v6.0.1-0d90135",
            "relay": "https://boost-relay.flashbots.net/eth/v1/builder/header/11035091/0xfc4140c6a665bfebdd841930a2b2abc728b981a3d1b61af0c0aa39ffcd0d50d6/0xb66941166ddf594deca81f707c0f8d60a5d62e222d52b54dbbee6079596d29e23afa43adac7f49bc064dac50af8d8e00",
            "bid_value": 0.008825908408675361
          }
        ],
        "payload_start_ms_into_slot": 0,
        "block_hash": "0xaa7a2f7c8ca66850d9e716d4dfc5b12cab2dbff8d09161af9dec9ebaef21929b"
      },
      "is_proxy_win": false,
      "is_winning_bid_highest": true,
      "el_reward_increase_wei": "0x0",
      "el_reward_increase_eth": 0.0,
      "onchain_bid_value": 0.00993944286898311,
      "second_highest_bid_value": 0.0,
      "onchain_bid_delivered_relay": "censoring.titanrelay.xyz",
      "second_higher_bid_delivered_relay": "",
      "is_payload_received": true,
      "el_reward_increase_percentage": 0,
      "el_reward_increase_percent_precise": 0.0,
      "equal_to_proxy_bidders": "https://censoring.titanrelay.xyz/eth/v1/builder/header/11035091/0xfc4140c6a665bfebdd841930a2b2abc728b981a3d1b61af0c0aa39ffcd0d50d6/0xb66941166ddf594deca81f707c0f8d60a5d62e222d52b54dbbee6079596d29e23afa43adac7f49bc064dac50af8d8e00",
      "is_equal_to_proxy_bid": true
    }
  }
}
```

### Relay Proxy and Relay data CSV input

For `mevboost_json` logs from MEV-boost >= v1.11.1 the `url` field was removed from `"bid received"` log lines, so all `bid.relay` fields are empty. The calculator first backfills relay attribution from `"best bid"` log entries; any remaining unlabelled bids can be supplemented using relay-side getheader CSV exports.

**`--relay-csv=<path>`**

Path to a CSV export of getheader requests received by the **non-proxy relay** (e.g. bloXroute relay direct).  
Expected columns (with header row): `slot`, `block_hash`, `value` (value column is present but ignored — see note below).  
Bids matched by `(slot, block_hash)` are labelled with the relay name derived from the file; at most as many bids are labelled as rows appear in the CSV for that key.

```bash
cargo run -- path/to/log.json mevboost_json csv \
  --relay-csv=/path/to/relay_getheader.csv
```

**`--proxy-relay-csv=<path>`**

Path to a CSV export of getheader requests received by the **relay-proxy**.  
Same format as `--relay-csv`. Used to label bids that the relay-proxy forwarded but that were not already resolved from `"best bid"` log entries.  
`--relay-csv` attribution takes priority over `--proxy-relay-csv` when both match the same `(slot, block_hash)`.

```bash
cargo run -- path/to/log.json mevboost_json csv \
  --proxy-relay-csv=/path/to/relay_proxy_getheader.csv
```

Both flags are optional and may be combined:

```bash
cargo run -- path/to/log.json mevboost_json csv \
  --relay-csv=/path/to/relay_getheader.csv \
  --proxy-relay-csv=/path/to/relay_proxy_getheader.csv
```

> **Note on `value` column precision**: the `value` column in the CSV is ignored for matching purposes. It is stored as `DOUBLE` in the source database, which introduces float64 rounding in the last 2–3 decimal digits relative to the exact string in MEV-boost logs. Matching is done on `(slot, block_hash)` only, which is sufficient for unambiguous attribution.

For now only `--proxy-relay-csv` is having effect on calculations since the relay bids are excluded from these which brought the uplift.

### Running for Figment
1. Go to https://github.com/bloXroute-Labs/loki-fetcher and run `python3 ./scripts/figment.py <path to csv report>`
2. Export relay proxy data for given month into CSV file:
```
SELECT 
	slot, 
	block_hash,
	block_value AS value
FROM 
	eth_mev.relay_proxy_provided_header 
WHERE  
  validator_id = 'figment' AND timestamp BETWEEN "2026-01-01 00:00:00" and "2026-01-31 23:59:59";
```
3. Run reward calculator:
```
RUST_LOG=debug cargo run <path to json report got on step 1> mevboost_json csv --proxy-relay-csv=<path to DB CSV export>
```