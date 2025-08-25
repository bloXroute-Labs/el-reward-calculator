use crate::log_source::types::{Bid, LogEntry};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use crate::{SlotInfo, SlotInfos};
use chrono::DateTime;
use url::Url;
use crate::Utc;
use ethers::types::U256;
use crate::log_source::common::is_relay_proxy;
use serde_json::{self, Deserializer, Value};
use rust_decimal_macros::dec;
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{Result as IoResult, Write};
use crate::mevboost_json::serde_json::to_writer_pretty;
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
    let _ = cleanup_slots_without_proxy(slot_infos);
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

                // DO NOT set block_hash here (headers aren’t blinded). We only add blinded
                // hashes via getPayload and only set `info.block_hash` from that list.
            }
        }
        "getPayload" => {
            if log_entry.message.msg == "received payload from relay" {
                slot_info.is_payload_received = true;

                // Treat payload blockHash as blinded and add to pending list.
                let bh = log_entry.message.blockHash.clone();
                if !bh.is_empty() && !slot_info.pending_blinded_block_hashes.contains(&bh) {
                    slot_info.pending_blinded_block_hashes.push(bh.clone());
                }

                // Only set block_hash from payload AND only if it's in the pending list (it is).
                if slot_info.info.block_hash.is_empty() && !bh.is_empty() {
                    slot_info.info.block_hash = bh;
                }

                // Optional: capture block number if present (merge_fields_from_log_entry leaves block_hash untouched)
                slot_info.merge_fields_from_log_entry(log_entry);

                debug!(
                    "[GETPAYLOAD] slot: {}, slot_uid: {}, payload (blinded) block_hash: {}",
                    slot, slot_uid, log_entry.message.blockHash
                );
            }
        }
        _ => {}
    }
}

pub fn finalize_slot_infos(slot_infos: &mut SlotInfos) {
    for (_slot, slot_map) in slot_infos.iter_mut() {
        for (slot_uid, slot_info) in slot_map.iter_mut() {
            // Sort bids descending by value (used by downstream checks)
            slot_info.info.bids.sort_by(|a, b| b.bid_value.cmp(&a.bid_value));

            // Invariant: clear any non-blinded leftovers before use
            if !slot_info.info.block_hash.is_empty()
                && !slot_info
                    .pending_blinded_block_hashes
                    .contains(&slot_info.info.block_hash)
            {
                debug!(
                    "[SANITY] Clearing non-blinded block_hash='{}' (uid={})",
                    slot_info.info.block_hash, slot_uid
                );
                slot_info.info.block_hash.clear();
            }

            // Resolve chosen hash strictly from pending blinded list and headers.
            // 1) If payload chose a blinded hash AND we have at least one header bid with it -> keep it.
            // 2) Else: among pending blinded hashes, pick the one that actually has header bids, max by bid_value.
            // 3) If still none -> skip EL calc for this UID.
            let chosen_hash = if !slot_info.info.block_hash.is_empty()
                && slot_info
                    .info
                    .bids
                    .iter()
                    .any(|b| b.block_hash == slot_info.info.block_hash)
            {
                slot_info.info.block_hash.clone()
            } else {
                // find best pending hash that exists in headers
                let mut best: Option<(String, Decimal)> = None;
                for ph in &slot_info.pending_blinded_block_hashes {
                    if ph.is_empty() {
                        continue;
                    }
                    if let Some(max_for_ph) = slot_info
                        .info
                        .bids
                        .iter()
                        .filter(|b| b.block_hash == *ph)
                        .map(|b| b.bid_value)
                        .max()
                    {
                        match best {
                            Some((_, ref cur)) if max_for_ph <= *cur => {}
                            _ => best = Some((ph.clone(), max_for_ph)),
                        }
                    }
                }
                if let Some((h, _)) = best {
                    h
                } else {
                    // No matching header for any blinded payload -> leave as-is, no EL calc.
                    slot_info.info.block_hash.clone()
                }
            };

            // If we still have no chosen hash, or chosen hash not present in headers, we skip EL.
            if chosen_hash.is_empty()
                || !slot_info
                    .info
                    .bids
                    .iter()
                    .any(|b| b.block_hash == chosen_hash)
            {
                debug!(
                    "[SKIP] No header matched any blinded payload hash (uid={}). No EL calc.",
                    slot_uid
                );
                // Zero-out EL/fee fields to be safe
                slot_info.onchain_bid_value = Decimal::ZERO;
                slot_info.is_proxy_win = false;
                slot_info.is_equal_to_proxy_bid = false;
                slot_info.equal_to_proxy_bidders.clear();
                slot_info.el_reward_increase_eth = Decimal::ZERO;
                slot_info.el_reward_increase_wei = U256::zero();
                slot_info.el_reward_increase_percent_precise = Decimal::ZERO;
                slot_info.el_reward_increase_percentage = 0;
                slot_info.second_highest_bid_value = Decimal::ZERO;
                slot_info.second_higher_bid_delivered_relay.clear();
                slot_info.onchain_bid_delivered_relay.clear();
                slot_info.is_winning_bid_highest = false;
                slot_info.fee_per_block = dec!(0.0);
                continue;
            }

            // Ensure invariants: chosen_hash must be in pending list (payload-blinded)
            if !slot_info
                .pending_blinded_block_hashes
                .contains(&chosen_hash)
            {
                // If a code path set it, don’t allow — clear and skip.
                debug!(
                    "[STRICT] Chosen hash '{}' not in pending blinded list (uid={}). Clearing and skipping.",
                    chosen_hash, slot_uid
                );
                slot_info.info.block_hash.clear();
                slot_info.onchain_bid_value = Decimal::ZERO;
                slot_info.is_proxy_win = false;
                slot_info.is_equal_to_proxy_bid = false;
                slot_info.equal_to_proxy_bidders.clear();
                slot_info.el_reward_increase_eth = Decimal::ZERO;
                slot_info.el_reward_increase_wei = U256::zero();
                slot_info.el_reward_increase_percent_precise = Decimal::ZERO;
                slot_info.el_reward_increase_percentage = 0;
                slot_info.second_highest_bid_value = Decimal::ZERO;
                slot_info.second_higher_bid_delivered_relay.clear();
                slot_info.onchain_bid_delivered_relay.clear();
                slot_info.is_winning_bid_highest = false;
                slot_info.fee_per_block = dec!(0.0);
                continue;
            }

            // Set the chosen blinded hash
            slot_info.info.block_hash = chosen_hash.clone();
            // Make sure pending includes it (should already)
            if !slot_info
                .pending_blinded_block_hashes
                .contains(&slot_info.info.block_hash)
            {
                slot_info
                    .pending_blinded_block_hashes
                    .push(slot_info.info.block_hash.clone());
            }

            // Winner bid = any header bid for chosen blinded hash.
            // If multiple relays carried the same block hash, pick the highest bid; tie-break lex host.
            let mut candidates: Vec<&Bid> = slot_info
                .info
                .bids
                .iter()
                .filter(|b| b.block_hash == slot_info.info.block_hash)
                .collect();

            candidates.sort_by(|a, b| {
                let o = b.bid_value.cmp(&a.bid_value);
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                host_from(&a.relay).cmp(&host_from(&b.relay))
            });

            let winner_bid = candidates[0];
            let onchain_val = winner_bid.bid_value;

            // Highest value in the slot (for win/tie checks)
            let slot_top_value = slot_info
                .info
                .bids
                .iter()
                .map(|b| b.bid_value)
                .max()
                .unwrap_or(Decimal::ZERO);

            // Who’s at the top?
            // Hosts (domain) for non-proxy that tie at top:
            let top_nonproxy_hosts: BTreeSet<String> = slot_info
                .info
                .bids
                .iter()
                .filter(|b| b.bid_value == slot_top_value && !is_relay_proxy(&b.relay))
                .map(|b| host_from(&b.relay))
                .collect();

            let top_proxy_exists = slot_info
                .info
                .bids
                .iter()
                .any(|b| b.bid_value == slot_top_value && is_relay_proxy(&b.relay));

            // Is the chosen winner one of the proxies and at the top with no non-proxy tie?
            let chosen_is_proxy = is_relay_proxy(&winner_bid.relay);
            let is_equal_to_proxy_bid = !top_nonproxy_hosts.is_empty() && top_proxy_exists;
            let is_proxy_win = chosen_is_proxy && (slot_top_value == onchain_val) && !is_equal_to_proxy_bid;

            // Delivered relay host for the chosen hash (lexicographically smallest among candidates matching hash & value)
            let mut chosen_hosts_for_hash: Vec<String> = candidates
                .iter()
                .filter(|b| b.bid_value == onchain_val)
                .map(|b| host_from(&b.relay))
                .collect();
            chosen_hosts_for_hash.sort();
            let delivered_host = chosen_hosts_for_hash
                .get(0)
                .cloned()
                .unwrap_or_else(|| host_from(&winner_bid.relay));

            // Second-best among non-proxies (< onchain value)
            let second_best_nonproxy = slot_info
                .info
                .bids
                .iter()
                .filter(|b| !is_relay_proxy(&b.relay) && b.bid_value < onchain_val)
                .max_by(|a, b| a.bid_value.cmp(&b.bid_value));

            let second_best_val = second_best_nonproxy.map(|b| b.bid_value).unwrap_or(Decimal::ZERO);
            let second_best_host = second_best_nonproxy
                .map(|b| host_from(&b.relay))
                .unwrap_or_default();

            // Fill fields
            slot_info.onchain_bid_value = onchain_val;
            slot_info.onchain_bid_delivered_relay = delivered_host;

            slot_info.is_winning_bid_highest = onchain_val == slot_top_value;
            slot_info.is_equal_to_proxy_bid = is_equal_to_proxy_bid;
            slot_info.equal_to_proxy_bidders = if is_equal_to_proxy_bid {
                top_nonproxy_hosts.iter().cloned().collect::<Vec<_>>().join(", ")
            } else {
                String::new()
            };
            slot_info.is_proxy_win = is_proxy_win;

            if is_proxy_win {
                slot_info.second_highest_bid_value = second_best_val;
                slot_info.second_higher_bid_delivered_relay = second_best_host;

                // Clamp negative uplift to zero
                let mut uplift = onchain_val - second_best_val;
                if uplift.is_sign_negative() {
                    uplift = Decimal::ZERO;
                }

                if uplift.is_zero() || onchain_val.is_zero() {
                    slot_info.el_reward_increase_wei = U256::zero();
                    slot_info.el_reward_increase_eth = Decimal::ZERO;
                    slot_info.el_reward_increase_percent_precise = Decimal::ZERO;
                    slot_info.el_reward_increase_percentage = 0;
                    slot_info.fee_per_block = dec!(0.0);
                } else {
                    let wei_multiplier = Decimal::from(1_000_000_000_000_000_000u128);
                    let uplift_wei_dec = (uplift * wei_multiplier).round();
                    let uplift_wei = U256::from_dec_str(&uplift_wei_dec.to_string()).unwrap_or(U256::zero());
                    let pct_precise = (uplift / onchain_val) * Decimal::from(100);

                    slot_info.el_reward_increase_wei = uplift_wei;
                    slot_info.el_reward_increase_eth = uplift;
                    slot_info.el_reward_increase_percent_precise = pct_precise;
                    slot_info.el_reward_increase_percentage = pct_precise.round().to_u64().unwrap_or(0);

                    // Fee tiers from positive uplift only
                    slot_info.fee_per_block = if pct_precise <= dec!(1) {
                        dec!(0.0)
                    } else if pct_precise <= dec!(5) {
                        if uplift >= dec!(0.0015) { dec!(0.0015) } else { dec!(0.0) }
                    } else if pct_precise <= dec!(9) {
                        if uplift > dec!(0.003) { dec!(0.003) }
                        else if uplift > dec!(0.0015) { dec!(0.0015) }
                        else { dec!(0.0) }
                    } else {
                        if uplift > dec!(0.005) { dec!(0.005) }
                        else if uplift > dec!(0.003) { dec!(0.003) }
                        else if uplift > dec!(0.0015) { dec!(0.0015) }
                        else { dec!(0.0) }
                    };
                }
            } else {
                // Proxy lost or tie with non-proxy -> zero uplift/fee
                slot_info.second_highest_bid_value = Decimal::ZERO;
                slot_info.second_higher_bid_delivered_relay.clear();
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
    /// Also: treat payload blockHash as blinded → push to `pending_blinded_block_hashes`.
    pub fn merge_fields_from_log_entry(&mut self, log_entry: &LogEntry) {
        // Payload-only block_hash assignment & pending maintenance
        if log_entry.message.method == "getPayload"
            && log_entry.message.msg == "received payload from relay"
            && !log_entry.message.blockHash.is_empty()
        {
            let bh = log_entry.message.blockHash.clone();

            if !self.pending_blinded_block_hashes.contains(&bh) {
                self.pending_blinded_block_hashes.push(bh.clone());
            }

            if self.info.block_hash.is_empty() {
                self.info.block_hash = bh;
            }
        }

        // Opportunistically capture block_number if provided and missing
        if self.block_number.is_empty() {
            if let Some(num) = log_entry.message.blockNumber {
                if num != 0 {
                    self.block_number = num.to_string();
                }
            }
        }
    }

    pub fn from_log_entry(log_entry: &LogEntry, slot_uid: String, slot: String) -> Self {
        let mut info = SlotInfo::new_with_slot_uid_and_slot(slot_uid, slot);
        info.merge_fields_from_log_entry(log_entry);
        info
    }
}

/// Remove all slot_uids that have no relay-proxy bids, return them as a map, and write them to JSON.
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

fn host_from(relay: &str) -> String {
    Url::parse(relay)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_else(|| relay.to_string())
}
