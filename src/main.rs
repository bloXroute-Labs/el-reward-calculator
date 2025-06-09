#![allow(unused_variables)]
mod log_source;
use log_source::mevboost_json;
use log_source::mevboost_text;
use log_source::commitboost_json;
use log_source::commitboost_text;
use log_source::vouch;
use csv::WriterBuilder;
use std::io::Write;
use chrono::Utc;
use serde::Deserialize;
use serde::Serialize;
use serde_json::{self};
use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::io::{self, ErrorKind};
use std::io::Result;
use std::str::FromStr;
use std::{env, i64};
use url::Url;
use ethers::types::U256;
use log::info;
use env_logger;
use log::debug;

//time="2024-10-09T09:17:12.404Z" level=info msg="submitBlindedBlock request start - 1404 milliseconds into slot 10136784"
// blockHash=0xae2c0d7e87e7eaeae842143db2970243e78a965f8390a4ea0f59de0b5403e78b
//     genesisTime=1606824023 method=getPayload msIntoSlot=1404
//     parentHash=0x8cff1dbd053fbc22fd3376fd863f644d8fe9ff4f32802ba090d0be1708ed1fda
//     slot=10136784 slotTimeSec=12 slotUID=9fc9db4c-46a1-493f-b9cd-7bc450c81e18
//     ua=Lighthouse/v5.3.0-d6ba8c3 version=1.8
//
// time="2024-09-27T17:15:00.988Z" level=info msg="received payload from relay"
// blockHash=0x012d8bb7b700313060ca620f96ba69a3fed405ddd9a959b6a4bb1e038bb94f89
// method=getPayload parentHash=0x70a835e90e4cae6c513d422f075eb22be7be0765d63b968340ff11f8d89b012b slot=10052773 slotUID=2fd9298d-f6fe-4a29-b303-57ebda0bee6b ua=Lighthouse/v5.3.0-d6ba8c3
// url="https://bloxroute.regulated.blxrbdn.com/eth/v1/builder/blinded_blocks" version=1.8
//test

#[derive(Debug,Default,Serialize, Deserialize)]
#[allow(dead_code)]
struct LogEntryVouch {
    pub level: String,
    pub service: String,
    #[serde(rename = "impl")]
    pub impl_field: String,
    pub slot: i64,
    pub provider: String,
    pub value: String,
    #[serde(rename = "value_delta")]
    pub value_delta: String,
    pub score: String,
    #[serde(rename = "score_delta")]
    pub score_delta: String,
    pub selected: bool,
    pub time: String,
    pub message: String,
}
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MEVBoostJSONLogEntry {
    pub level: String,
    pub method: String,
    pub msg: String,
    pub slot: String,
    #[serde(rename = "slotUID", default)]
    pub slot_uid: String,
    pub time: String,
    #[serde(default)]
    pub block_hash: String,
    #[serde(default)]
    pub parent_hash: String,
    #[serde(default)]
    pub ua: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub version: String,
    // Optional only needed for `getHeader` -> bid received
    #[serde(default)]
    pub block_number: Option<i64>,
    #[serde(default)]
    pub pubkey: Option<String>,
    #[serde(default)]
    pub tx_root: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug,Default,Serialize, Deserialize)]
#[allow(dead_code)]
struct LogEntry {
    client: String,
    filename: String,
    hostname: String,
    log_format_version: String,
    logstamp: u64,
    message: Message,
    network: String,
    syslog_identifier: String,
}

#[derive(Debug,Default,Serialize, Deserialize)]
#[allow(non_snake_case, dead_code)]
struct Message {
    blockHash: String,
    level: String,
    method: String,
    msg: String,
    parentHash: String,
    slot: String,
    slotUID: String,
    time: String,
    ua: String,
    url: Option<String>,
    version: String,
    blockNumber: Option<u64>,
    pubkey: Option<String>,
    txRoot: Option<String>,
    value: Option<String>,
}

#[derive(Debug,Default,Serialize, Deserialize)]
struct SlotInfo {
    slot_uid: String,
    slot: String,
    block_number: String,
    info: RequestInfo,
    is_proxy_win: bool,
    is_winning_bid_highest: bool,
    el_reward_increase_wei: U256,
    el_reward_increase_eth: f64,
    onchain_bid_value: f64,
    second_highest_bid_value: f64,
    onchain_bid_delivered_relay: String,
    second_higher_bid_delivered_relay: String,
    is_payload_received: bool,
    el_reward_increase_percentage: u64,
    el_reward_increase_percent_precise: f64,
    equal_to_proxy_bidders: String,
    is_equal_to_proxy_bid: bool,
    fee_per_block: f64,
}


#[derive(Debug, Default, Serialize, Deserialize, PartialEq, PartialOrd)]
struct RequestInfo {
    header_start_ms_into_slot: i64,
    bids: Vec<Bid>,
    payload_start_ms_into_slot: i64,
    block_hash: String,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, PartialOrd, Clone)]
struct Bid {
    timestamp: i64,
    pubkey: String,
    block_hash: String,
    parent_hash: String,
    block_number: String,
    slot: String,
    ua: String,
    relay: String,
    bid_value: f64,
}


#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
struct SlotInfoWithoutBids<'a> {
    slot_uid: &'a str,
    slot: &'a str,
    block_number: &'a str,
    header_start_ms_into_slot: i64,
    payload_start_ms_into_slot: i64,
    block_hash: &'a str,
    is_proxy_win: bool,
    is_winning_bid_highest: bool,
    el_reward_increase_eth: f64,
    el_reward_increase_wei: U256,
    onchain_bid_value: f64,
    onchain_bid_delivered_relay: String,
    second_highest_bid_value: f64,
    second_higher_bid_delivered_relay: String,
    is_payload_received: bool,
    el_reward_increase_percentage: u64,
    el_reward_increase_percent_precise: f64,
    equal_to_proxy_bidders: String,
    is_equal_to_proxy_bid: bool,
    fee_per_block: f64,
}


#[derive(Debug)]
enum LogSource {
    MevboostJson,
    MevboostText,
    CommitboostJson,
    CommitboostText,
    Vouch,
}

impl FromStr for LogSource {
    type Err = io::Error;
    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        match input.to_lowercase().as_str() {
            "mevboost_json" => Ok(LogSource::MevboostJson),
            "mevboost_text" => Ok(LogSource::MevboostText),
            "commitboost_json" => Ok(LogSource::CommitboostJson),
            "commitboost_text" => Ok(LogSource::CommitboostText),
            "vouch" => Ok(LogSource::Vouch),
            _ => Err(io::Error::new(
                ErrorKind::Other,
                format!("Invalid LogSource '{}'. Must be one of mevboost_json, mevboost_text, commitboost_json, commitboost_text, vouch.", input),
            )),
        }
    }
}


type SlotInfos = HashMap<String, HashMap<String, SlotInfo>>; // slot -> (slot_uid -> SlotInfo)

fn main() -> Result<()> {
    // Set default log level if RUST_LOG is not set.
    if env::var("RUST_LOG").is_err() {
        env::set_var("RUST_LOG", "info");
    }
    env_logger::init();

    // Reading a file via CLI.
    let mut slot_infos: SlotInfos = HashMap::new();
    let args: Vec<String> = env::args().collect();
    if args.len() < 3 {
        eprint!("either filename or validator client id is missing");
        std::process::exit(1);
    }
    let filename = &args[1];
    let validator_client_id_flag = &args[2];
    let output_format = args.get(3).map(|s| s.as_str()).unwrap_or("json");
    info!("{:?} - {:?} - {:?} - {:?}", args, filename, validator_client_id_flag, output_format);

    let log_source = match LogSource::from_str(validator_client_id_flag) {
        Ok(source) => source,
        Err(err) => {
            eprintln!("{}", err);
            std::process::exit(1);
        }
    };
    let file = File::open(filename)?;
    let reader = BufReader::new(file);

    match log_source {
        LogSource::MevboostJson => {
            mevboost_json::parse_file_content(reader, &mut slot_infos);
        }
        LogSource::MevboostText => {
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        mevboost_text::process_lines(line, &mut slot_infos);
                    }
                    Err(e) => eprintln!("failed to read lines: {}", e),
                }
            }
        }
        LogSource::CommitboostJson => {
            commitboost_json::parse_file_content(reader, &mut slot_infos);
        }
        LogSource::CommitboostText => {
            for line in reader.lines() {
                match line {
                    Ok(line) => {
                        commitboost_text::process_lines(line, &mut slot_infos);
                    }
                    Err(e) => eprintln!("failed to read lines: {}", e),
                }
            }
        }
        LogSource::Vouch => {
            vouch::parse_file_content(reader, &mut slot_infos);
        }
    }

    let now = Utc::now();
    let date_str = Utc::now().format("%d_%m_%Y").to_string();
    let time_str = now.format("%H_%M_%S").to_string();

    // Create folder path.
    let folder_path = format!("slot_infos/{}/", date_str);
    fs::create_dir_all(&folder_path)?;

    if output_format == "csv" {
        let file_path = format!("{}/slot_infos_{}_{}.csv", folder_path, date_str, time_str);
        let file = File::create(&file_path)?;
        let mut wtr = WriterBuilder::new().has_headers(true).from_writer(file);

        // For each slot, select one record per rules.
        for (slot, slot_info_with_uid) in slot_infos {
            debug!("Slot: {}, records: {:?}", slot, slot_info_with_uid);

            // Choose the record as follows:
            // If any record has is_proxy_win true, select the one with highest el_reward_increase_eth.
            // Otherwise, choose the first record.
            let chosen = if let Some(proxy_wins) = {
                let wins: Vec<&SlotInfo> = slot_info_with_uid.values().filter(|si| si.is_proxy_win).collect();
                if !wins.is_empty() { Some(wins) } else { None }
            } {
                proxy_wins.into_iter().max_by(|a, b| {
                    a.el_reward_increase_eth
                        .partial_cmp(&b.el_reward_increase_eth)
                        .unwrap_or(std::cmp::Ordering::Equal)
                })
            } else {
                slot_info_with_uid.values().next()
            };

            if let Some(slot_info) = chosen {
                let record = SlotInfoWithoutBids {
                    slot_uid: &slot_info.slot_uid,
                    slot: &slot_info.slot,
                    block_number: &slot_info.block_number,
                    header_start_ms_into_slot: slot_info.info.header_start_ms_into_slot,
                    payload_start_ms_into_slot: slot_info.info.payload_start_ms_into_slot,
                    block_hash: &slot_info.info.block_hash,
                    is_proxy_win: slot_info.is_proxy_win,
                    is_winning_bid_highest: slot_info.is_winning_bid_highest,
                    el_reward_increase_eth: slot_info.el_reward_increase_eth,
                    el_reward_increase_wei: slot_info.el_reward_increase_wei.clone(),
                    onchain_bid_value: slot_info.onchain_bid_value,
                    onchain_bid_delivered_relay: slot_info.onchain_bid_delivered_relay.clone(),
                    second_highest_bid_value: slot_info.second_highest_bid_value,
                    second_higher_bid_delivered_relay: slot_info.second_higher_bid_delivered_relay.clone(),
                    is_payload_received: slot_info.is_payload_received,
                    el_reward_increase_percentage: slot_info.el_reward_increase_percentage,
                    el_reward_increase_percent_precise: slot_info.el_reward_increase_percent_precise,
                    equal_to_proxy_bidders: slot_info.equal_to_proxy_bidders.clone(),
                    is_equal_to_proxy_bid: slot_info.is_equal_to_proxy_bid,
                    fee_per_block: slot_info.fee_per_block,
                };
                wtr.serialize(record)?;
            }
        }
        wtr.flush()?;
    } else {
        let file_path = format!("{}/slot_infos_{}_{}.json", folder_path, date_str, time_str);
        let mut file = File::create(&file_path)?;
        let json_data = serde_json::to_string_pretty(&slot_infos)?;
        file.write_all(json_data.as_bytes())?;
    }
    Ok(())
}


impl SlotInfo {
    fn new(slot_uid: String) -> Self {
        Self {
            slot_uid,
            slot: String::new(),
            block_number: String::new(),
            info: Default::default(),
            is_proxy_win: false,
            el_reward_increase_wei: U256::default(),
            el_reward_increase_eth: 0.0,
            onchain_bid_value: 0.0,
            second_highest_bid_value: 0.0,
            onchain_bid_delivered_relay: String::new(),
            second_higher_bid_delivered_relay: String::new(),
            is_winning_bid_highest: false,
            is_payload_received: false,
            el_reward_increase_percentage: 0,
            el_reward_increase_percent_precise: 0.0,
            equal_to_proxy_bidders:String::new(),
            is_equal_to_proxy_bid: false,
            fee_per_block:0.0,
        }
    }

    // initializes with both slot_uid and slot
    #[allow(dead_code)]
    fn new_with_slot_uid_and_slot(slot_uid: String, slot: String) -> Self {
        Self {
            slot_uid,
            slot,
            block_number: String::new(),
            info: Default::default(),
            is_proxy_win: false,
            el_reward_increase_wei: U256::default(),
            el_reward_increase_eth: 0.0,
            onchain_bid_value: 0.0,
            second_highest_bid_value: 0.0,
            onchain_bid_delivered_relay: String::new(),
            second_higher_bid_delivered_relay: String::new(),
            is_winning_bid_highest: false,
            is_payload_received: false,
            el_reward_increase_percentage: 0,
            el_reward_increase_percent_precise: 0.0,
            equal_to_proxy_bidders:String::new(),
            is_equal_to_proxy_bid: false,
            fee_per_block:0.0,
        }
    }

}


fn parse_url(bid: &Bid) -> String {
    // Remove leading/trailing backslashes and quotes
    let trimmed = bid.relay.trim_matches('\\').trim_matches('"');
    match Url::parse(trimmed) {
        Ok(parsed_url) => parsed_url.host_str().unwrap_or("").to_string(),
        Err(e) => {
            eprintln!("Failed to parse URL from relay field '{}': {} : {}", trimmed, e,bid.relay.clone());
            trimmed.to_string()
        },
    }
}
