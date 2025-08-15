use crate::log_source::types::{Bid,  LogEntry};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use crate::{SlotInfo, SlotInfos};
use chrono::{DateTime, Utc};
use url::Url;
use ethers::types::U256;
use crate::log_source::common::is_relay_proxy;
use serde_json::{self, Deserializer, Value};
use rust_decimal_macros::dec;
use std::collections::{HashMap,BTreeSet};
use log::debug;

pub fn parse_file_content<R: std::io::Read>(reader: R, slot_infos: &mut SlotInfos) {
    let stream = Deserializer::from_reader(reader).into_iter::<Value>();
    for entry in stream {
        match entry {
            Ok(Value::Object(map)) => {
                match serde_json::from_value::<LogEntry>(Value::Object(map)) {
                    Ok(log_entry) => {
                        process_json(&log_entry, slot_infos);
                    }
                    Err(e) => {
                        eprintln!("Failed to parse log entry: {}. Skipping.", e);
                    }
                }
            }
            Ok(Value::Null) => eprintln!("Encountered Null value. Skipping."),
            Ok(Value::Bool(_)) => eprintln!("Encountered Boolean value. Skipping."),
            Ok(Value::Number(_)) => eprintln!("Encountered Number value. Skipping."),
            Ok(Value::String(_)) => eprintln!("Encountered String value. Skipping."),
            Ok(Value::Array(vec)) => {
                for item in vec {
                    match serde_json::from_value::<LogEntry>(item) {
                        Ok(log_entry) => process_json(&log_entry, slot_infos),
                        Err(e) => eprintln!("Failed to parse log entry: {}. Skipping.", e),
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to parse JSON entry: {}. Skipping.", e);
            }
        }
    }
    cleanup_slots_without_proxy(slot_infos);
    finalize_slot_infos(slot_infos)
}

fn process_json(log_entry: &LogEntry, slot_infos: &mut SlotInfos) {
    let slot = log_entry.message.slot.clone();
    let slot_uid = log_entry.message.slotUID.clone();
    let slot_info_map = slot_infos.entry(slot.clone()).or_insert_with(HashMap::new);

    // Ensure merging happens if slot_uid already exists
    let slot_info = slot_info_map
        .entry(slot_uid.clone())
        .and_modify(|existing| existing.merge_fields_from_log_entry(log_entry))
        .or_insert_with(|| SlotInfo::from_log_entry(log_entry, slot_uid.clone(), slot.clone()));

    match log_entry.message.method.as_str() {
        "getHeader" => {
            if log_entry.message.msg == "bid received" {
                let mut bid: Bid = Default::default();
                let date = DateTime::parse_from_rfc3339(&log_entry.message.time)
                    .expect(&format!("failed to parse timestamp for slot-{}, timestamp-{}", slot.clone(), log_entry.message.time.clone()))
                    .with_timezone(&Utc);
                bid.timestamp = date.to_utc().timestamp();
                bid.slot = log_entry.message.slot.clone();
                bid.block_hash = log_entry.message.blockHash.clone();
                bid.parent_hash = log_entry.message.parentHash.clone();
                bid.ua = log_entry.message.ua.clone();
                bid.relay = log_entry.message.url.as_ref().unwrap_or(&String::new()).clone();
                bid.pubkey = log_entry.message.pubkey.as_ref().unwrap_or(&String::new()).clone();
                bid.block_number = log_entry.message.blockNumber.map_or(String::new(), |num| num.to_string());
                bid.bid_value = log_entry.message.value.as_deref().unwrap_or("0.0").parse::<Decimal>().unwrap_or(Decimal::ZERO);

                slot_info.info.bids.push(bid);

                // Opportunistic merge (e.g., block_number if provided in the header line)
                slot_info.merge_fields_from_log_entry(log_entry);
            }
        }
        "getPayload" => {
            if log_entry.message.msg == "received payload from relay" {
                // We already have `slot_info`; use it directly and opportunistically merge fields
                slot_info.is_payload_received = true;

                // If we haven't locked a winner yet, set from payload now
                if slot_info.info.block_hash.is_empty() && !log_entry.message.blockHash.is_empty() {
                    slot_info.info.block_hash = log_entry.message.blockHash.clone();
                }

                // Merge any additional fields (e.g., blockNumber if present in this log)
                slot_info.merge_fields_from_log_entry(log_entry);

                debug!(
                    "[GETPAYLOD] slot: {}, slot_uid: {}, payload block_hash: {}",
                    slot, slot_uid, log_entry.message.blockHash
                );
                // Remaining calculations deferred to finalize_slot_infos()
            }
        }
        _ => {}
    }
}

pub fn finalize_slot_infos(slot_infos: &mut SlotInfos) {
    for (slot, slot_map) in slot_infos.iter_mut() {
        for (slot_uid, slot_info) in slot_map.iter_mut() {
            // Sort bids descending by bid value (used by several downstream checks)
            slot_info.info.bids.sort_by(|a, b| b.bid_value.cmp(&a.bid_value));

            // 1) If we do not have a winning block hash yet (no payload or payload missing block hash),
            //    choose the best available bid with a non-empty block hash (Commit-Boost-style fallback).
            if slot_info.info.block_hash.is_empty() {
                if let Some(best_bid) = slot_info.info.bids.iter().find(|b| !b.block_hash.is_empty()) {
                    debug!(
                        "[AUTO-MATCH] No payload-set block_hash; falling back to best bid block_hash={} (slot_uid={})",
                        best_bid.block_hash, slot_uid
                    );
                    slot_info.info.block_hash = best_bid.block_hash.clone();
                } else {
                    debug!(
                        "[SKIP] Slot {} (uid: {}) has no payload block_hash and no bids with a block hash",
                        slot, slot_uid
                    );
                    continue;
                }
            }

            let winning_block_hash = &slot_info.info.block_hash;
            debug!("[FINALIZE] slot: {}, slot_uid: {}, payload/winning block_hash: {}", slot, slot_uid, winning_block_hash);

            // 2) Try to find a bid that matches the chosen block hash.
            let mut winner_index: Option<usize> = None;
            for (i, bid) in slot_info.info.bids.iter().enumerate() {
                if bid.block_hash == *winning_block_hash {
                    winner_index = Some(i);
                    break;
                }
            }

            // 3) If no bid matched the payload hash, fallback to the highest bid with a non-empty hash.
            if winner_index.is_none() {
                if let Some(best_bid_idx) = slot_info.info.bids.iter().position(|b| !b.block_hash.is_empty()) {
                    let best_bid = &slot_info.info.bids[best_bid_idx];
                    debug!(
                        "[AUTO-MATCH] No bid matched payload block_hash {}; falling back to best bid block_hash={} (slot_uid={})",
                        winning_block_hash, best_bid.block_hash, slot_uid
                    );
                    slot_info.info.block_hash = best_bid.block_hash.clone();
                    winner_index = Some(best_bid_idx);
                } else {
                    debug!(
                        "[SKIP] No bid matched payload block_hash {} and no non-empty bid hashes to fallback (slot_uid={})",
                        winning_block_hash, slot_uid
                    );
                    continue;
                }
            }

            // Safe to unwrap now
            let winner_idx = winner_index.unwrap();
            let bid = &slot_info.info.bids[winner_idx];

            // Keep/merge block_number if present
            if slot_info.block_number.is_empty() && !bid.block_number.is_empty() {
                slot_info.block_number = bid.block_number.clone();
            }

            // Highest-bid logic
            let highest_bid = slot_info.info.bids.get(0).unwrap();
            let highest_bidders: BTreeSet<_> = slot_info.info.bids
                .iter()
                .filter(|b| b.bid_value == highest_bid.bid_value)
                .map(|b| b.relay.clone())
                .collect();

            let relay_proxy_won = highest_bidders.iter().any(|relay| is_relay_proxy(relay));
            let relay_proxy_bidders: Vec<String> = if relay_proxy_won {
                highest_bidders
                    .iter()
                    .filter(|relay| !is_relay_proxy(relay))
                    .map(|relay| Url::parse(relay).ok().and_then(|url| url.host_str().map(String::from)).unwrap_or_default())
                    .collect()
            } else {
                Vec::new()
            };

            let highest_bidder_urls: Vec<String> = highest_bidders
                .iter()
                .map(|relay| Url::parse(relay).ok().and_then(|url| url.host_str().map(String::from)).unwrap_or_default())
                .collect();

            slot_info.onchain_bid_delivered_relay = highest_bidder_urls.join(", ");
            slot_info.onchain_bid_value = bid.bid_value.clone();
            slot_info.equal_to_proxy_bidders = relay_proxy_bidders.join(", ");
            slot_info.is_equal_to_proxy_bid = !relay_proxy_bidders.is_empty();
            slot_info.is_proxy_win = relay_proxy_won && !slot_info.is_equal_to_proxy_bid;

            slot_info.is_winning_bid_highest = bid.block_hash == highest_bid.block_hash
                || slot_info.info.bids.iter().any(|b| b.block_hash == *winning_block_hash && b.bid_value == highest_bid.bid_value);

            if highest_bidders.len() > 1 && !relay_proxy_won {
                debug!("[FINALIZE] Skipping EL reward calc: multiple highest bids and proxy did not win.");
                continue;
            }

            if slot_info.is_proxy_win {
                let second_best_bid = slot_info.info.bids.iter().skip(1).find(|b| !is_relay_proxy(&b.relay));
                let second_best_val = second_best_bid.map_or(Decimal::ZERO, |b| b.bid_value);
                slot_info.second_highest_bid_value = second_best_val;
                slot_info.second_higher_bid_delivered_relay = second_best_bid.map_or(String::new(), |bid| {
                    Url::parse(&bid.relay).ok().and_then(|url| url.host_str().map(String::from)).unwrap_or_default()
                });

                if !slot_info.is_equal_to_proxy_bid && second_best_val > Decimal::ZERO {
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

                    slot_info.fee_per_block = if el_reward_percent_precise <= dec!(1) {
                        dec!(0.0)
                    } else if el_reward_percent_precise <= dec!(5) {
                        if el_reward_increase >= dec!(0.0015) {
                            dec!(0.0015)
                        } else {
                            dec!(0.0)
                        }
                    } else if el_reward_percent_precise <= dec!(9) {
                        if el_reward_increase > dec!(0.003) {
                            dec!(0.003)
                        } else if el_reward_increase > dec!(0.0015) {
                            dec!(0.0015)
                        } else {
                            dec!(0.0)
                        }
                    } else {
                        if el_reward_increase > dec!(0.005) {
                            dec!(0.005)
                        } else if el_reward_increase > dec!(0.003) {
                            dec!(0.003)
                        } else if el_reward_increase > dec!(0.0015) {
                            dec!(0.0015)
                        } else {
                            dec!(0.0)
                        }
                    };
                }
            }
        }
    }
}

impl SlotInfo {
    pub fn merge_fields_from_log_entry(&mut self, log_entry: &LogEntry) {
        // opportunistically fill in missing block_hash/block_number
        if self.info.block_hash.is_empty() {
            if !log_entry.message.blockHash.is_empty() {
                self.info.block_hash = log_entry.message.blockHash.clone();
            }
        }
        if self.block_number.is_empty() {
            if let Some(num) = log_entry.message.blockNumber {
                if num != 0 {
                    self.block_number = num.to_string();
                }
            }
        }
        // extend later if needed (e.g., parent hash, pubkey, etc.)
    }

    pub fn from_log_entry(log_entry: &LogEntry, slot_uid: String, slot: String) -> Self {
        let mut info = SlotInfo::new_with_slot_uid_and_slot(slot_uid, slot);
        info.merge_fields_from_log_entry(log_entry);
        info
    }
}

pub fn cleanup_slots_without_proxy(slot_infos: &mut SlotInfos) {
    let mut total_slot_count = slot_infos.len();
    let mut slots_checked = 0;
    let mut slots_removed = 0;
    let mut slot_uids_removed = 0;
    let mut slots_to_remove: Vec<(String, String)> = Vec::new();

    for (slot, slot_map) in slot_infos.iter() {
        slots_checked += 1;
        for (slot_uid, slot_info) in slot_map.iter() {
            let has_proxy_bid = slot_info.info.bids.iter().any(|bid| is_relay_proxy(&bid.relay));
            if !has_proxy_bid {
                println!(
                    "[Cleanup] No relay-proxy bid found for slot_uid '{}', slot '{}'. Marking for removal.",
                    slot_uid, slot
                );
                slots_to_remove.push((slot.clone(), slot_uid.clone()));
            } else {
                println!(
                    "[Cleanup] Found at least one relay-proxy bid for slot_uid '{}', slot '{}'. Keeping.",
                    slot_uid, slot
                );
            }
        }
    }

    for (slot, slot_uid) in &slots_to_remove {
        if let Some(slot_map) = slot_infos.get_mut(slot) {
            if slot_map.remove(slot_uid).is_some() {
                println!(
                    "[Cleanup] Removed slot_uid '{}' from slot '{}'",
                    slot_uid, slot
                );
                slot_uids_removed += 1;
            }

            if slot_map.is_empty() {
                if slot_infos.remove(slot).is_some() {
                    println!(
                        "[Cleanup] Removed entire slot '{}' since no slot_uids remain",
                        slot
                    );
                    slots_removed += 1;
                    total_slot_count -= 1;
                }
            }
        }
    }

    println!(
        "[Cleanup Summary] Checked {} slots, removed {} slot_uids from {} slots. Remaining slots: {}.",
        slots_checked, slot_uids_removed, slots_removed, total_slot_count
    );
}
