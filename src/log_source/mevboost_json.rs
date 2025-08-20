use crate::log_source::types::{Bid, LogEntry};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use crate::{SlotInfo, SlotInfos};
use chrono::{DateTime};
use url::Url;
 use crate::Utc;
use ethers::types::U256;
use crate::log_source::common::is_relay_proxy;
use serde_json::{self, Deserializer, Value};
use rust_decimal_macros::dec;
use std::collections::{ BTreeSet};
use log::debug;
use std::collections::{HashMap};
use std::fs::{self, File};
use std::io::{Result as IoResult, Write};
use crate::mevboost_json::serde_json::to_writer_pretty;

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
   let _ =  cleanup_slots_without_proxy(slot_infos);
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
                    .expect(&format!(
                        "failed to parse timestamp for slot-{}, timestamp-{}",
                        slot.clone(),
                        log_entry.message.time.clone()
                    ))
                    .with_timezone(&Utc);
                bid.timestamp = date.to_utc().timestamp();
                bid.slot = log_entry.message.slot.clone();
                bid.block_hash = log_entry.message.blockHash.clone();
                bid.parent_hash = log_entry.message.parentHash.clone();
                bid.ua = log_entry.message.ua.clone();
                bid.relay = log_entry.message.url.as_ref().unwrap_or(&String::new()).clone();
                bid.pubkey = log_entry.message.pubkey.as_ref().unwrap_or(&String::new()).clone();
                bid.block_number =
                    log_entry.message.blockNumber.map_or(String::new(), |num| num.to_string());
                bid.bid_value = log_entry
                    .message
                    .value
                    .as_deref()
                    .unwrap_or("0.0")
                    .parse::<Decimal>()
                    .unwrap_or(Decimal::ZERO);

                slot_info.info.bids.push(bid);

                // Opportunistic merge (e.g., block_number if provided in the header line).
                // NOTE: merge_fields_from_log_entry will NOT set block_hash from headers (payload-only).
                slot_info.merge_fields_from_log_entry(log_entry);
            }
        }
        "getPayload" => {
            if log_entry.message.msg == "received payload from relay" {
                slot_info.is_payload_received = true;

                // Only set block_hash from payload (handled inside merge_fields_from_log_entry)
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
            // Sort bids descending by value (used by several downstream checks)
            slot_info.info.bids.sort_by(|a, b| b.bid_value.cmp(&a.bid_value));
            if slot_info.info.bids.is_empty() {
                continue;
            }

            // Determine top bid value and a preferred "top hash":
            // prefer a top hash from a relay-proxy (if present), else any top bidder’s hash
            let max_val = slot_info.info.bids[0].bid_value;
            let top_hash = slot_info
                .info
                .bids
                .iter()
                .filter(|b| b.bid_value == max_val)
                .find(|b| is_relay_proxy(&b.relay))
                .map(|b| b.block_hash.clone())
                .or_else(|| {
                    slot_info
                        .info
                        .bids
                        .iter()
                        .find(|b| b.bid_value == max_val)
                        .map(|b| b.block_hash.clone())
                });

            // If we do not have a payload-set block hash, initialize via top_hash or first non-empty hash
            if slot_info.info.block_hash.is_empty() {
                if let Some(h) = top_hash.clone().filter(|h| !h.is_empty()) {
                    slot_info.info.block_hash = h;
                } else if let Some(best_with_hash) =
                    slot_info.info.bids.iter().find(|b| !b.block_hash.is_empty())
                {
                    slot_info.info.block_hash = best_with_hash.block_hash.clone();
                } else {
                    debug!(
                        "[SKIP] Slot {} (uid: {}) has no payload hash and no bid hash.",
                        slot, slot_uid
                    );
                    continue;
                }
            }

            // If payload-matched bid value is LOWER than the top value, override to the top hash
            if let Some(th) = top_hash.clone().filter(|h| !h.is_empty()) {
                let payload_val = slot_info
                    .info
                    .bids
                    .iter()
                    .find(|b| b.block_hash == slot_info.info.block_hash)
                    .map(|b| b.bid_value)
                    .unwrap_or(Decimal::ZERO);

                if payload_val < max_val && th != slot_info.info.block_hash {
                    debug!(
                        "[OVERRIDE] payload hash {} (val {}) < top val {}; replacing with {} (slot_uid={})",
                        slot_info.info.block_hash, payload_val, max_val, th, slot_uid
                    );
                    slot_info.info.block_hash = th;
                }
            }

            // Find the winning bid by the chosen hash
            let winning_block_hash = slot_info.info.block_hash.clone();
            let winner_index = slot_info
                .info
                .bids
                .iter()
                .position(|b| b.block_hash == winning_block_hash);

            let winner_idx = if let Some(i) = winner_index {
                i
            } else if let Some(i) = slot_info.info.bids.iter().position(|b| !b.block_hash.is_empty())
            {
                // fallback to any non-empty hash
                slot_info.info.block_hash = slot_info.info.bids[i].block_hash.clone();
                i
            } else {
                debug!(
                    "[SKIP] No bid matches chosen hash and no alternative hash available (uid={})",
                    slot_uid
                );
                continue;
            };

            let bid = &slot_info.info.bids[winner_idx];

            // Keep/merge block_number if present
            if slot_info.block_number.is_empty() && !bid.block_number.is_empty() {
                slot_info.block_number = bid.block_number.clone();
            }

            // Highest-bidder group (by value) for winner classification
            let highest_bid = &slot_info.info.bids[0];
            let highest_bidders: BTreeSet<_> = slot_info
                .info
                .bids
                .iter()
                .filter(|b| b.bid_value == highest_bid.bid_value)
                .map(|b| b.relay.clone())
                .collect();

            let relay_proxy_won = highest_bidders.iter().any(|r| is_relay_proxy(r));
            let relay_proxy_bidders: Vec<String> = if relay_proxy_won {
                highest_bidders
                    .iter()
                    .filter(|r| !is_relay_proxy(r))
                    .map(|r| {
                        Url::parse(r)
                            .ok()
                            .and_then(|u| u.host_str().map(String::from))
                            .unwrap_or_default()
                    })
                    .collect()
            } else {
                Vec::new()
            };

            let highest_bidder_urls: Vec<String> = highest_bidders
                .iter()
                .map(|r| {
                    Url::parse(r)
                        .ok()
                        .and_then(|u| u.host_str().map(String::from))
                        .unwrap_or_default()
                })
                .collect();

            // Populate fields using the chosen winner `bid`
            slot_info.onchain_bid_delivered_relay = highest_bidder_urls.join(", ");
            slot_info.onchain_bid_value = bid.bid_value;
            slot_info.equal_to_proxy_bidders = relay_proxy_bidders.join(", ");
            slot_info.is_equal_to_proxy_bid = !relay_proxy_bidders.is_empty();
            slot_info.is_proxy_win = relay_proxy_won && !slot_info.is_equal_to_proxy_bid;

            // Is the chosen winner also in the highest-value group?
            slot_info.is_winning_bid_highest =
                (bid.bid_value == highest_bid.bid_value)
                    || slot_info
                        .info
                        .bids
                        .iter()
                        .any(|b| b.block_hash == winning_block_hash && b.bid_value == highest_bid.bid_value);

            if highest_bidders.len() > 1 && !relay_proxy_won {
                debug!("[FINALIZE] Multiple highest bids, proxy did not win; skipping EL calc.");
                continue;
            }

            // === EL uplift + fee calc (non-negative clamp) ===
            if slot_info.is_proxy_win {
                // Best non-proxy competitor among remaining bids
                let second_best_bid = slot_info
                    .info
                    .bids
                    .iter()
                    .filter(|b| !is_relay_proxy(&b.relay))
                    .max_by(|a, b| a.bid_value.cmp(&b.bid_value));

                let second_best_val = second_best_bid.map_or(Decimal::ZERO, |b| b.bid_value);
                slot_info.second_highest_bid_value = second_best_val;
                slot_info.second_higher_bid_delivered_relay = second_best_bid
                    .map_or(String::new(), |b| {
                        Url::parse(&b.relay)
                            .ok()
                            .and_then(|u| u.host_str().map(String::from))
                            .unwrap_or_default()
                    });

                // Clamp negative uplift to zero
                let mut el_reward_increase = slot_info.onchain_bid_value - second_best_val;
                if el_reward_increase.is_sign_negative() {
                    el_reward_increase = Decimal::ZERO;
                }

                if el_reward_increase.is_zero() || slot_info.onchain_bid_value.is_zero() {
                    slot_info.el_reward_increase_wei = U256::zero();
                    slot_info.el_reward_increase_eth = Decimal::ZERO;
                    slot_info.el_reward_increase_percent_precise = Decimal::ZERO;
                    slot_info.el_reward_increase_percentage = 0;
                    slot_info.fee_per_block = dec!(0.0);
                } else {
                    let wei_multiplier = Decimal::from(1_000_000_000_000_000_000u128);
                    let el_reward_increase_wei_decimal =
                        (el_reward_increase * wei_multiplier).round();
                    let el_reward_increase_wei = U256::from_dec_str(
                        &el_reward_increase_wei_decimal.to_string(),
                    )
                    .unwrap_or(U256::zero());

                    let el_reward_percent_precise =
                        (el_reward_increase / slot_info.onchain_bid_value) * Decimal::from(100);

                    slot_info.el_reward_increase_wei = el_reward_increase_wei;
                    slot_info.el_reward_increase_eth = el_reward_increase;
                    slot_info.el_reward_increase_percent_precise = el_reward_percent_precise;
                    slot_info.el_reward_increase_percentage =
                        el_reward_percent_precise.round().to_u64().unwrap_or(0);

                    // Fee tiers from positive uplift only
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
            } else {
                // Proxy didn't win or was a tie -> no uplift/fee
                slot_info.el_reward_increase_wei = U256::zero();
                slot_info.el_reward_increase_eth = Decimal::ZERO;
                slot_info.el_reward_increase_percent_precise = Decimal::ZERO;
                slot_info.el_reward_increase_percentage = 0;
                slot_info.fee_per_block = dec!(0.0);
            }
        }
    }
}

impl SlotInfo {
    /// Only set `block_hash` from payload lines; never from header lines.
    pub fn merge_fields_from_log_entry(&mut self, log_entry: &LogEntry) {
        // Payload-only block_hash assignment
        if self.info.block_hash.is_empty()
            && log_entry.message.method == "getPayload"
            && log_entry.message.msg == "received payload from relay"
            && !log_entry.message.blockHash.is_empty()
        {
            self.info.block_hash = log_entry.message.blockHash.clone();
        }

        // Opportunistically capture block_number if provided and missing
        if self.block_number.is_empty() {
            if let Some(num) = log_entry.message.blockNumber {
                if num != 0 {
                    self.block_number = num.to_string();
                }
            }
        }
        // Extend later if needed (e.g., parent hash, pubkey, etc.)
    }

    pub fn from_log_entry(log_entry: &LogEntry, slot_uid: String, slot: String) -> Self {
        let mut info = SlotInfo::new_with_slot_uid_and_slot(slot_uid, slot);
        info.merge_fields_from_log_entry(log_entry);
        info
    }
}


/// Remove all slot_uids that have no relay-proxy bids, return them as a map, and write them to JSON.
///
/// - `slot_infos` is modified in place (entries without proxy bids are removed).
/// - Returns a `SlotInfos` containing only the removed entries (grouped by slot).
/// - Also writes the removed map to `removed_json_path` as pretty JSON.
///
pub fn cleanup_slots_without_proxy(slot_infos: &mut SlotInfos) -> IoResult<SlotInfos> {
    let mut total_slot_count = slot_infos.len();
    let mut slots_checked = 0;
    let mut slots_removed = 0;
    let mut slot_uids_removed = 0;

    // Collect (slot, slot_uid) pairs to remove in a first pass
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

    // This will hold everything we removed, grouped by slot
    let mut removed: SlotInfos = HashMap::new();

    // Second pass: actually remove and move into `removed`
    for (slot, slot_uid) in &slots_to_remove {
        if let Some(slot_map) = slot_infos.get_mut(slot) {
            if let Some(removed_info) = slot_map.remove(slot_uid) {
                println!(
                    "[Cleanup] Removed slot_uid '{}' from slot '{}'",
                    slot_uid, slot
                );
                removed
                    .entry(slot.clone())
                    .or_insert_with(HashMap::new)
                    .insert(slot_uid.clone(), removed_info);

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

    // ----- Write outputs under the same dated folder as stats writer -----
    let now = Utc::now();
    let date_str = now.format("%d_%m_%Y").to_string();
    let time_str = now.format("%H_%M_%S").to_string();

    // Ensure folder: slot_infos/<date>/
    let dir_path = format!("slot_infos/{}/", date_str);
    fs::create_dir_all(&dir_path)?;

    // Proper FILE paths (no trailing slash after .json/.txt)
    let json_path = format!("{}nonproxy_slots_{}_{}.json", dir_path, date_str, time_str);
    let summary_path = format!("{}nonproxy_slots_summary_{}_{}.txt", dir_path, date_str, time_str);

    // JSON dump
    let file = File::create(&json_path)?;
    to_writer_pretty(file, &removed)?;
    println!("[Cleanup] Wrote removed no-proxy entries to '{}'", json_path);

    // Small summary
    let removed_slots_count = removed.len();
    let removed_slot_uids_count: usize = removed.values().map(|m| m.len()).sum();

    let mut sfile = File::create(&summary_path)?;
    writeln!(sfile, "Removed (no relay-proxy bids) summary")?;
    writeln!(sfile, "-----------------------------------")?;
    writeln!(sfile, "Slots checked            : {}", slots_checked)?;
    writeln!(sfile, "Slots before cleanup     : {}", total_slot_count + slots_removed)?;
    writeln!(sfile, "Slots removed            : {}", slots_removed)?;
    writeln!(sfile, "Slot UIDs removed        : {}", removed_slot_uids_count)?;
    writeln!(sfile, "Distinct slots removed   : {}", removed_slots_count)?;
    writeln!(sfile, "Remaining slots          : {}", total_slot_count)?;
    println!("[Cleanup] Wrote removed summary to '{}'", summary_path);

    Ok(removed)

}
