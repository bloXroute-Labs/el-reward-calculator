
use serde::{Deserialize, Serialize};
use crate::{ SlotInfo, SlotInfos };
use serde_json::{self, Deserializer, Value};
use chrono::{DateTime, Utc};
use std::collections::{HashMap,BTreeSet};
use crate::Bid;
use ethers::types::U256;
use ethers::utils::parse_ether;
use ethers::utils::format_units;
use crate::log_source::common::is_relay_proxy;
use log::debug;

pub fn parse_file_content<R: std::io::Read>(reader : R , slot_infos :&mut SlotInfos){
    let stream = Deserializer::from_reader(reader).into_iter::<Value>();
    for entry in stream {
        match entry {
            Ok(Value::Object(map)) => {
                match serde_json::from_value::<CommitBoostLogEntry>(Value::Object(map)) {
                    Ok(log_entry) => {
                        // Process the valid log entry
                        process_json(&log_entry, slot_infos);
                    }
                    Err(e) => {
                        eprintln!("Failed to parse log entry: {}. Skipping.", e);
                    }
                }
            }
            Ok(Value::Null) => {
                eprintln!("Encountered Null value. Skipping.");
            }
            Ok(Value::Bool(_)) => {
                eprintln!("Encountered Boolean value. Skipping.");
            }
            Ok(Value::Number(_)) => {
                eprintln!("Encountered Number value. Skipping.");
            }
            Ok(Value::String(_)) => {
                eprintln!("Encountered String value. Skipping.");
            }
            Ok(Value::Array(vec)) => {
                for item in vec {
                    match serde_json::from_value::<CommitBoostLogEntry>(item) {
                        Ok(log_entry) => {
                            // Process the valid log entry
                            process_json(&log_entry, slot_infos);
                        }
                        Err(e) => {
                            eprintln!("Failed to parse log entry: {}. Skipping.", e);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to parse JSON entry: {}. Skipping.", e);
                // Optionally, log the error or take other actions
            }
        }
    }
    // process all the slot infos
    post_process_all_slots(slot_infos)
}

fn process_json(log_entry: &CommitBoostLogEntry, slot_infos: &mut SlotInfos) {
    let span = &log_entry.span;

    if !matches!(
        span.method.as_str(),
        "/eth/v1/builder/blinded_blocks" |
        "/eth/v1/builder/header/{slot}/{parent_hash}/{pubkey}"
    ) {
        return;
    }

    let slot = span.slot.unwrap_or_default().to_string();
    let slot_uid = span.req_id.clone().unwrap_or_default();

    debug!(
        "[PROCESS] method={}, slot={}, slot_uid={}",
        span.method, slot, slot_uid
    );

    let slot_info_map = slot_infos.entry(slot.clone()).or_insert_with(HashMap::new);
    let slot_info = slot_info_map.entry(slot_uid.clone()).or_insert_with(|| {
        debug!("[INIT] Creating SlotInfo for slot_uid: {}", slot_uid);
        SlotInfo::new_with_slot_uid_and_slot(slot_uid.clone(), slot.clone())
    });

    match span.method.as_str() {
        "/eth/v1/builder/header/{slot}/{parent_hash}/{pubkey}" => {
            if log_entry.fields.message.as_deref() == Some("received new header") {
                let mut bid: Bid = Default::default();

                let date = DateTime::parse_from_rfc3339(&log_entry.timestamp)
                    .expect(&format!(
                        "Failed to parse timestamp for slot {}, timestamp {}",
                        slot, log_entry.timestamp
                    ))
                    .with_timezone(&Utc);

                bid.timestamp = date.timestamp();
                bid.slot = slot.clone();
                bid.block_hash = log_entry.fields.block_hash.clone().unwrap_or_default();
                bid.bid_value = log_entry.fields.value_eth
                    .as_deref()
                    .unwrap_or("0.0")
                    .parse()
                    .unwrap_or(0.0);
                bid.relay = log_entry.fields.relay_id.clone().unwrap_or_default();

                debug!(
                    "[GETHEADER] Adding bid: slot={}, block_hash={}, value_eth={}",
                    bid.slot, bid.block_hash, bid.bid_value
                );

                slot_info.info.bids.push(bid);
            }
        }

        "/eth/v1/builder/blinded_blocks" => {
            if log_entry.fields.message.as_deref() == Some("received unblinded block") {
                let block_hash = log_entry.fields.block_hash.clone().unwrap_or_default();
                debug!(
                    "[SUBMIT] Processing submit_blinded_block: slot={}, block_hash={}",
                    slot, block_hash
                );

                slot_info.is_payload_received = true;
                slot_info.info.block_hash = block_hash;
            }
        }

        _ => {}
    }
}

pub fn post_process_all_slots(slot_infos: &mut SlotInfos) {
    for (slot, slot_info_map) in slot_infos.iter_mut() {
        for (slot_uid, slot_info) in slot_info_map.iter_mut() {
            if slot_info.info.bids.is_empty() {
                continue;
            }

            slot_info.info.bids.sort_by(|a, b| b.bid_value.total_cmp(&a.bid_value));
            debug!("[SUBMIT] Sorted bids for slot_uid={} by bid_value descending", slot_uid);
            if let Some(highest_bid) = slot_info.info.bids.get(0) {
                let highest_bidders: BTreeSet<_> = slot_info.info.bids
                    .iter()
                    .filter(|b| b.bid_value == highest_bid.bid_value)
                    .map(|b| b.relay.clone())
                    .collect();

                let block_hash = &slot_info.info.block_hash;
                let winning_bid = slot_info.info.bids.iter().find(|b| &b.block_hash == block_hash);

                if let Some(bid) = winning_bid {
                    let relay_proxy_won = highest_bidders.iter().any(|relay| is_relay_proxy(relay));

                    let relay_proxy_bidders: Vec<String> = if relay_proxy_won {
                        highest_bidders.iter()
                            .filter(|relay| !is_relay_proxy(relay))
                            .map(|relay| relay.to_string())
                            .collect()
                    } else {
                        Vec::new()
                    };

                    let highest_bidder_urls: Vec<String> = highest_bidders.iter()
                        .map(|relay| relay.to_string())
                        .collect();

                    slot_info.onchain_bid_delivered_relay = highest_bidder_urls.join(", ");
                    slot_info.onchain_bid_value = bid.bid_value;
                    slot_info.equal_to_proxy_bidders = relay_proxy_bidders.join(", ");
                    slot_info.is_equal_to_proxy_bid = !relay_proxy_bidders.is_empty();
                    slot_info.is_proxy_win = relay_proxy_won && !slot_info.is_equal_to_proxy_bid;

                    slot_info.is_winning_bid_highest =
                        bid.block_hash == highest_bid.block_hash
                        || slot_info.info.bids.iter().any(|b| b.block_hash == bid.block_hash && b.bid_value == highest_bid.bid_value);

                    if highest_bidders.len() > 1 && !relay_proxy_won {
                        debug!("[POST] Skipping slot_uid={} due to multiple highest bids and relay proxy not winning.", slot_uid);
                        continue;
                    }

                    if slot_info.is_proxy_win {
                        let second_best_bid = slot_info.info.bids.iter()
                            .skip(1)
                            .find(|bid| !is_relay_proxy(&bid.relay));

                        slot_info.second_highest_bid_value = second_best_bid.map_or(0.0, |b| b.bid_value);
                        slot_info.second_higher_bid_delivered_relay = second_best_bid
                            .map(|b| b.relay.clone())
                            .unwrap_or_default();

                        if !slot_info.is_equal_to_proxy_bid && slot_info.second_highest_bid_value > 0.0 {
                            let el_reward_increase = slot_info.onchain_bid_value - slot_info.second_highest_bid_value;
                            let el_reward_increase_wei: U256 = parse_ether(&el_reward_increase.to_string()).expect("Invalid Ether value");
                            let el_reward_increase_eth = format_units(el_reward_increase_wei, "ether")
                                .expect("Formatting failed")
                                .parse::<f64>()
                                .unwrap();
                            let el_reward_increase_percentage = ((el_reward_increase / slot_info.onchain_bid_value) * 100.0).round() as u64;

                            debug!(
                                "[POST] EL reward increase for slot_uid {}: {} ETH ({}%)",
                                slot_uid, el_reward_increase_eth, el_reward_increase_percentage
                            );

                            slot_info.el_reward_increase_wei = el_reward_increase_wei;
                            slot_info.el_reward_increase_eth = el_reward_increase_eth;
                            slot_info.el_reward_increase_percentage = el_reward_increase_percentage;
                        }
                    }
                }
            }
        }
    }
}

#[derive(Debug, Default, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct CommitBoostLogEntry {
    pub timestamp: String,
    pub level: String,
    pub message: String,
    pub span: Span,

    #[serde(flatten)]
    pub fields: FlatFields,  // flattened fields like value_eth, block_hash, etc.
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FlatFields {
    pub message: Option<String>,
    pub latency: Option<String>,
    pub value_eth: Option<String>,
    pub block_hash: Option<String>,
    pub relay_id: Option<String>,
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Span {
    #[serde(rename = "req_id")]
    pub req_id: Option<String>,
    pub slot: Option<i64>,
    pub name: String,
    pub method: String,
    pub parent_hash: Option<String>,
    pub block_hash: Option<String>,
    pub block_number: Option<u64>,
    pub validator: Option<String>,
}
