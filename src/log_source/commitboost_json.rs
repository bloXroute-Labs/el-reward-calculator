
use serde::{Deserialize, Serialize};
use crate::{ CommitBoostSlotInfos};
use serde_json::{self, Deserializer, Value};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use crate::log_source::types::{Bid,CommitBoostRequest, CommitBoostSlotInfo};
use ethers::types::U256;
use ethers::utils::parse_ether;
use crate::log_source::common::is_relay_proxy;
use log::debug;

pub fn parse_file_content<R: std::io::Read>(reader: R, slot_infos: &mut CommitBoostSlotInfos) {
    let stream = Deserializer::from_reader(reader).into_iter::<Value>();
    for entry in stream {
        match entry {
            Ok(Value::Object(map)) => {
                match serde_json::from_value::<CommitBoostLogEntry>(Value::Object(map)) {
                    Ok(log_entry) => {
                        process_json(&log_entry, slot_infos);
                    }
                    Err(e) => eprintln!("Failed to parse log entry: {}. Skipping.", e),
                }
            }
            Ok(Value::Array(vec)) => {
                for item in vec {
                    match serde_json::from_value::<CommitBoostLogEntry>(item) {
                        Ok(log_entry) => process_json(&log_entry, slot_infos),
                        Err(e) => eprintln!("Failed to parse log entry: {}. Skipping.", e),
                    }
                }
            }
            _ => eprintln!("Unsupported JSON entry encountered. Skipping."),
        }
    }
    post_process_all_slots(slot_infos);
}

fn process_json(log_entry: &CommitBoostLogEntry, slot_infos: &mut CommitBoostSlotInfos) {
    let span = &log_entry.span;
    let slot = span.slot.unwrap_or_default().to_string();
    let parent_hash = span.parent_hash.clone().unwrap_or_else(|| "unknown".to_string());
    let slot_uid = format!("{}_{}", slot, parent_hash);

    let slot_info_map = slot_infos.entry(slot.clone()).or_insert_with(HashMap::new);
    let slot_info = slot_info_map.entry(slot_uid.clone()).or_insert_with(|| {
        debug!("[INIT] Creating CommitBoostSlotInfo for slot_uid: {}", slot_uid);
        CommitBoostSlotInfo::new(slot_uid.clone(), slot.clone())
    });

    match span.method.as_str() {
        "/eth/v1/builder/header/{slot}/{parent_hash}/{pubkey}" => {
            if log_entry.message == "received new header" {
                let req_id = span.req_id.clone().unwrap_or_else(|| "unknown_reqid".to_string());
                let mut bid: Bid = Default::default();

                let date = DateTime::parse_from_rfc3339(&log_entry.timestamp)
                    .unwrap()
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

                slot_info
                    .requests
                    .entry(req_id.clone())
                    .or_insert_with(Default::default)
                    .bids
                    .push(bid);
            }
        }
        "/eth/v1/builder/blinded_blocks" => {
            if log_entry.message == "received unblinded block" {
                let block_hash = span.block_hash.clone().unwrap_or_default();

                // Match all requests that contain the block_hash
                let mut matched_req_ids: Vec<(&String, &CommitBoostRequest)> = slot_info
                    .requests
                    .iter()
                    .filter(|(_, req)| req.bids.iter().any(|b| b.block_hash == block_hash))
                    .collect();

                if !matched_req_ids.is_empty() {
                    // Sort by highest bid among relays matching the block_hash
                    matched_req_ids.sort_by(|(_, a), (_, b)| {
                        let a_max = a.bids.iter()
                            .filter(|b| b.block_hash == block_hash)
                            .map(|b| b.bid_value)
                            .fold(0.0, f64::max);
                        let b_max = b.bids.iter()
                            .filter(|b| b.block_hash == block_hash)
                            .map(|b| b.bid_value)
                            .fold(0.0, f64::max);
                        b_max.total_cmp(&a_max)
                    });

                    let (best_req_id, _) = matched_req_ids[0];

                    if slot_info.selected_req_id.is_none() || slot_info.block_hash != block_hash {
                        debug!(
                            "[SUBMIT] Selected best matching req_id={} with highest bid for block_hash={}",
                            best_req_id, block_hash
                        );
                        slot_info.selected_req_id = Some(best_req_id.clone());
                        slot_info.block_hash = block_hash;
                        slot_info.block_number = format!("{}", span.block_number.unwrap_or_default());
                    }
                } else {
                    debug!(
                        "[SUBMIT] No matching request for block_hash {} in slot_uid {}",
                        block_hash, slot_uid
                    );
                }
            }
        }
        _ => {}
    }
}

pub fn post_process_all_slots(slot_infos: &mut CommitBoostSlotInfos) {
    for (_slot, slot_info_map) in slot_infos.iter_mut() {
        for (_slot_uid, slot_info) in slot_info_map.iter_mut() {
            let selected_req_id = match &slot_info.selected_req_id {
                Some(id) => id,
                None => continue,
            };

            let bidset = match slot_info.requests.get(selected_req_id) {
                Some(b) => b,
                None => continue,
            };

            let mut bids = bidset.bids.clone();
            if bids.is_empty() {
                continue;
            }

            bids.sort_by(|a, b| b.bid_value.total_cmp(&a.bid_value));
            let highest_bid = &bids[0];

            let winning_bid = bids.iter().find(|b| &b.block_hash == &slot_info.block_hash);

            if let Some(bid) = winning_bid {
                let winning_relay = bid.relay.clone();
                let relay_proxy_won = is_relay_proxy(&winning_relay);

                slot_info.onchain_bid_delivered_relay = winning_relay.clone();
                slot_info.onchain_bid_value = bid.bid_value;
                slot_info.is_proxy_win = relay_proxy_won;
                slot_info.is_equal_to_proxy_bid = false; // reset
                slot_info.equal_to_proxy_bidders = String::new();

                slot_info.is_winning_bid_highest =
                    bid.block_hash == highest_bid.block_hash
                    || bids.iter().any(|b| b.block_hash == bid.block_hash && b.bid_value == highest_bid.bid_value);

                if relay_proxy_won {
                    let second_best_bid = bids.iter()
                        .filter(|b| !is_relay_proxy(&b.relay))
                        .find(|b| b.bid_value < bid.bid_value);

                    slot_info.second_highest_bid_value = second_best_bid.map_or(0.0, |b| b.bid_value);
                    slot_info.second_higher_bid_delivered_relay = second_best_bid.map_or(String::new(), |b| b.relay.clone());

                    if slot_info.second_highest_bid_value > 0.0 {
                        let el_reward_increase = slot_info.onchain_bid_value - slot_info.second_highest_bid_value;
                        let el_reward_increase_eth = (el_reward_increase * 1e18f64).round() / 1e18f64;
                        let el_reward_increase_wei: U256 = parse_ether(&format!("{:.18}", el_reward_increase_eth)).unwrap_or_default();

                        let precise_percent = (el_reward_increase / slot_info.onchain_bid_value) * 100.0;
                        let rounded_percent_precise = (precise_percent * 100.0).round() / 100.0;
                        let el_reward_increase_percentage = precise_percent.round() as u64;

                        slot_info.el_reward_increase_wei = el_reward_increase_wei;
                        slot_info.el_reward_increase_eth = el_reward_increase_eth;
                        slot_info.el_reward_increase_percentage = el_reward_increase_percentage;
                        slot_info.el_reward_increase_percent_precise = rounded_percent_precise;
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
    pub fields: FlatFields, // flattened fields like value_eth, block_hash, etc.
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct FlatFields {
    pub latency: Option<String>,
    pub value_eth: Option<String>,
    pub block_hash: Option<String>,
    pub relay_id: Option<String>,
    pub version: Option<String>,  // from getHeader log
}

#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
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
