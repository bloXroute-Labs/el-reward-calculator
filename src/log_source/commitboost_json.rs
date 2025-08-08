
use serde::{Deserialize, Serialize};
use crate::{ CommitBoostSlotInfos};
use serde_json::{self, Deserializer, Value};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use crate::log_source::types::{Bid,CommitBoostRequest, CommitBoostSlotInfo, SlotTrait};
use ethers::types::U256;
use crate::log_source::common::is_relay_proxy;
use log::debug;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use url::Url;

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
}

fn process_json(log_entry: &CommitBoostLogEntry, slot_infos: &mut CommitBoostSlotInfos) {
    let span = &log_entry.span;
    let slot = span.slot.unwrap_or_default().to_string();
    let parent_hash = span.parent_hash.clone().unwrap_or_else(|| "unknown".to_string());
    let slot_uid = format!("{}_{}", slot, parent_hash);

    let slot_info_map = slot_infos.entry(slot.clone()).or_insert_with(HashMap::new);

    // Ensure merging happens if slot_uid already exists
    let slot_info = slot_info_map
        .entry(slot_uid.clone())
        .and_modify(|existing| existing.merge_fields_from_log_entry(log_entry))
        .or_insert_with(|| {
            debug!("[INIT] Creating CommitBoostSlotInfo for slot_uid: {}", slot_uid);
            CommitBoostSlotInfo::from_log_entry(log_entry, slot_uid.clone(), slot.clone())
        });

    match span.method.as_str() {
        "/eth/v1/builder/header/{slot}/{parent_hash}/{pubkey}" => {
            if log_entry.message == "received new header" {
                let req_id = span.req_id.clone().unwrap_or_else(|| "unknown_reqid".to_string());

                let mut bid: Bid = Default::default();
                if let Ok(date) = DateTime::parse_from_rfc3339(&log_entry.timestamp) {
                    bid.timestamp = date.with_timezone(&Utc).timestamp();
                }

                bid.slot = slot.clone();
                bid.block_hash = log_entry.fields.block_hash.clone().unwrap_or_default();
                bid.bid_value = log_entry
                    .fields
                    .value_eth
                    .as_deref()
                    .unwrap_or("0.0")
                    .parse::<Decimal>()
                    .unwrap_or(Decimal::ZERO);
                bid.relay = log_entry.fields.relay_id.clone().unwrap_or_default();

                slot_info
                    .requests
                    .entry(req_id.clone())
                    .or_insert_with(Default::default)
                    .bids
                    .push(bid.clone());

                // Handle resolution of earlier unmatched blinded block
                if slot_info.selected_req_id.is_none()
                    && slot_info.pending_blinded_block_hashes.contains(&bid.block_hash)
                {
                    debug!(
                        "[RESOLVE] Found pending blinded block hash {} via header; setting selected_req_id={}",
                        bid.block_hash, req_id
                    );
                    slot_info.selected_req_id = Some(req_id);
                    slot_info.block_hash = bid.block_hash.clone();
                }
            }
        }

        "/eth/v1/builder/blinded_blocks" => {
            if log_entry.message == "received unblinded block" {
                let block_hash = span.block_hash.clone().unwrap_or_default();

                let mut matched_req_ids: Vec<(&String, &CommitBoostRequest)> = slot_info
                    .requests
                    .iter()
                    .filter(|(_, req)| req.bids.iter().any(|b| b.block_hash == block_hash))
                    .collect();

                if !matched_req_ids.is_empty() {
                    matched_req_ids.sort_by(|(aid, a), (bid, b)| {
                        let a_max = a
                            .bids
                            .iter()
                            .filter(|b| b.block_hash == block_hash)
                            .map(|b| b.bid_value)
                            .fold(Decimal::ZERO, Decimal::max);

                        let b_max = b
                            .bids
                            .iter()
                            .filter(|b| b.block_hash == block_hash)
                            .map(|b| b.bid_value)
                            .fold(Decimal::ZERO, Decimal::max);

                        b_max.cmp(&a_max).then_with(|| aid.cmp(bid))
                    });

                    let (best_req_id, _) = matched_req_ids[0];

                    if slot_info.selected_req_id.is_none() || slot_info.get_block_hash() != block_hash {
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
                        "[DEFER] No matching request yet for blinded block {}; storing for later in slot_uid={}",
                        block_hash, slot_uid
                    );
                    if !slot_info.pending_blinded_block_hashes.contains(&block_hash) {
                        slot_info.pending_blinded_block_hashes.push(block_hash);
                    }
                }
            }
        }

        _ => {}
    }
}

impl CommitBoostSlotInfo {
    pub fn merge_fields_from_log_entry(&mut self, log_entry: &CommitBoostLogEntry) {
        if self.block_hash.is_empty() {
            if let Some(bh) = &log_entry.fields.block_hash {
                self.block_hash = bh.clone();
            }
        }

        if self.block_number.is_empty() {
            if let Some(num) = log_entry.span.block_number {
                if num != 0 {
                    self.block_number = num.to_string();
                }
            }
        }

        // Optional: can extend to merge more fields later if needed
    }

    pub fn from_log_entry(log_entry: &CommitBoostLogEntry, slot_uid: String, slot: String) -> Self {
        let mut info = CommitBoostSlotInfo::new(slot_uid, slot);
        info.merge_fields_from_log_entry(log_entry);
        info
    }
}


pub fn post_process_all_slots(slot_infos: &mut CommitBoostSlotInfos) {
    let mut slots: Vec<_> = slot_infos.keys().cloned().collect();
    slots.sort();

    for slot in slots {
        if let Some(slot_info_map) = slot_infos.get_mut(&slot) {
            let mut slot_uids: Vec<_> = slot_info_map.keys().cloned().collect();
            slot_uids.sort();

            for slot_uid in slot_uids {
                if let Some(slot_info) = slot_info_map.get_mut(&slot_uid) {
                    // 1. Attempt late match using pending_blinded_block_hashes
                    if slot_info.selected_req_id.is_none() && !slot_info.pending_blinded_block_hashes.is_empty() {
                        for blinded_block_hash in &slot_info.pending_blinded_block_hashes {
                            let mut matched: Vec<(&String, &CommitBoostRequest)> = slot_info
                                .requests
                                .iter()
                                .filter(|(_, req)| req.bids.iter().any(|b| &b.block_hash == blinded_block_hash))
                                .collect();

                            if !matched.is_empty() {
                                matched.sort_by(|(aid, a), (bid, b)| {
                                    let a_max = a
                                        .bids
                                        .iter()
                                        .filter(|b| &b.block_hash == blinded_block_hash)
                                        .map(|b| b.bid_value)
                                        .fold(Decimal::ZERO, Decimal::max);

                                    let b_max = b
                                        .bids
                                        .iter()
                                        .filter(|b| &b.block_hash == blinded_block_hash)
                                        .map(|b| b.bid_value)
                                        .fold(Decimal::ZERO, Decimal::max);

                                    b_max.cmp(&a_max).then_with(|| aid.cmp(bid))
                                });

                                let (best_req_id, _) = matched[0];
                                debug!(
                                    "[FINALIZE] Late match for blinded block hash {} -> req_id {}",
                                    blinded_block_hash, best_req_id
                                );
                                slot_info.selected_req_id = Some(best_req_id.clone());
                                slot_info.block_hash = blinded_block_hash.clone();
                                break;
                            }
                        }
                    }

                    // 2. If still none, try best bid across all requests
                    if slot_info.selected_req_id.is_none() {
                        let mut best_bid: Option<(String, String, Decimal)> = None;

                        for (req_id, req) in &slot_info.requests {
                            for bid in &req.bids {
                                if !bid.block_hash.is_empty() && bid.bid_value > Decimal::ZERO {
                                    if let Some((_, _, current_max)) = &best_bid {
                                        if bid.bid_value > *current_max {
                                            best_bid = Some((req_id.clone(), bid.block_hash.clone(), bid.bid_value));
                                        }
                                    } else {
                                        best_bid = Some((req_id.clone(), bid.block_hash.clone(), bid.bid_value));
                                    }
                                }
                            }
                        }

                        if let Some((best_req_id, block_hash, _)) = best_bid {
                            debug!(
                                "[AUTO-MATCH] Selected best bid req_id={} block_hash={} (fallback)",
                                best_req_id, block_hash
                            );
                            slot_info.selected_req_id = Some(best_req_id);
                            slot_info.block_hash = block_hash;
                        }
                    }

                    // Continue with reward calculation only if match found
                    let selected_req_id = match &slot_info.selected_req_id {
                        Some(id) => id,
                        None => {
                            debug!(
                                "[SKIP] Slot {} (uid: {}) has no selected_req_id and no valid blinded/bid fallback match",
                                slot_info.slot, slot_info.slot_uid
                            );
                            continue;
                        }
                    };

                    let bidset = match slot_info.requests.get(selected_req_id) {
                        Some(b) => b,
                        None => {
                            debug!(
                                "[SKIP] selected_req_id {} not found in slot_uid {}",
                                selected_req_id, slot_uid
                            );
                            continue;
                        }
                    };

                    let mut bids = bidset.bids.clone();
                    if bids.is_empty() {
                        debug!(
                            "[SKIP] No bids for selected_req_id {} in slot_uid {}",
                            selected_req_id, slot_uid
                        );
                        continue;
                    }

                    bids.sort_by(|a, b| b.bid_value.cmp(&a.bid_value));
                    let highest_bid = &bids[0];

                    let winning_bid = bids.iter().find(|b| &b.block_hash == &slot_info.block_hash);

                    if let Some(bid) = winning_bid {
                        let winning_relay = bid.relay.clone();
                        let relay_proxy_won = is_relay_proxy(&winning_relay);

                        slot_info.onchain_bid_delivered_relay = winning_relay.clone();
                        slot_info.onchain_bid_value = bid.bid_value;
                        slot_info.is_proxy_win = relay_proxy_won;
                        slot_info.is_equal_to_proxy_bid = false;
                        slot_info.equal_to_proxy_bidders = String::new();

                        slot_info.is_winning_bid_highest =
                            bid.block_hash == highest_bid.block_hash
                            || bids.iter().any(|b| b.block_hash == bid.block_hash && b.bid_value == highest_bid.bid_value);

                        if relay_proxy_won {
                            let second_best_bid = bids.iter()
                                .filter(|b| !is_relay_proxy(&b.relay))
                                .find(|b| b.bid_value < bid.bid_value);

                            let second_best_val = second_best_bid.map_or(Decimal::ZERO, |b| b.bid_value);
                            slot_info.second_highest_bid_value = second_best_val;
                            slot_info.second_higher_bid_delivered_relay = second_best_bid.map_or(String::new(), |bid| {
                                Url::parse(&bid.relay).ok().and_then(|url| url.host_str().map(String::from)).unwrap_or_default()
                            });

                            if second_best_val > Decimal::ZERO {
                                let el_reward_increase = slot_info.onchain_bid_value - second_best_val;
                                let wei_multiplier = Decimal::from(1_000_000_000_000_000_000u128);
                                let el_reward_increase_wei_decimal = (el_reward_increase * wei_multiplier).round();
                                let el_reward_increase_wei: U256 = U256::from_dec_str(&el_reward_increase_wei_decimal.to_string()).unwrap_or(U256::zero());

                                let el_reward_percent_precise = if slot_info.onchain_bid_value > Decimal::ZERO {
                                    (el_reward_increase / slot_info.onchain_bid_value) * Decimal::from(100)
                                } else {
                                    Decimal::ZERO
                                };

                                slot_info.el_reward_increase_wei = el_reward_increase_wei;
                                slot_info.el_reward_increase_eth = el_reward_increase;
                                slot_info.el_reward_increase_percent_precise = el_reward_percent_precise;
                                slot_info.el_reward_increase_percentage = el_reward_percent_precise.round().to_u64().unwrap_or(0);
                            }
                        }
                    } else {
                        debug!(
                            "[SKIP] No bid matched block_hash {} in slot_uid {}",
                            slot_info.block_hash, slot_uid
                        );
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
