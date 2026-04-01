use crate::log_source::types::{Bid, LogEntry};
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use crate::{SlotInfo, SlotInfos};
use chrono::{DateTime, SecondsFormat, Utc};
use url::Url;
use ethers::types::U256;
use crate::log_source::common::{is_relay_proxy, get_slot_start_time_utc};
use serde_json::{self, Deserializer, Value};
use rust_decimal_macros::dec;
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{Result as IoResult, Write};
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
                    Err(e) => eprintln!("Failed to parse log entry: {}. Skipping.", e),
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
            Ok(_) => {
                // Non-object JSON: ignore
            }
            Err(e) => eprintln!("Failed to parse JSON entry: {}. Skipping.", e),
        }
    }

    // First, drop UIDs that never had any relay-proxy bid at all (diagnostic & cleanup).
    let _ = cleanup_slots_without_proxy(slot_infos);

    // Then compute per-slot classification, applying results strictly per-UID.
    finalize_slot_infos(slot_infos);
}

// ----------------------------- Parsing -------------------------------------

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

                // Timestamp -> epoch seconds
                let date = DateTime::parse_from_rfc3339(&log_entry.message.time)
                    .unwrap_or_else(|_| {
                        panic!(
                            "failed to parse timestamp for slot-{}, timestamp-{}",
                            slot, log_entry.message.time
                        )
                    });
                let date_utc = date.with_timezone(&Utc);
                bid.timestamp = date_utc.timestamp();

                bid.slot         = log_entry.message.slot.clone();
                bid.block_hash   = log_entry.message.blockHash.clone();
                bid.parent_hash  = log_entry.message.parentHash.clone();
                bid.ua           = log_entry.message.ua.clone();
                bid.relay        = log_entry.message.url.as_deref().unwrap_or("").to_string();
                bid.pubkey       = log_entry.message.pubkey.as_deref().unwrap_or("").to_string();
                bid.block_number = log_entry.message.blockNumber
                    .map_or(String::new(), |num| num.to_string());
                bid.bid_value    = log_entry.message.value.as_deref().unwrap_or("0.0")
                    .parse::<Decimal>().unwrap_or(Decimal::ZERO);

                slot_info.info.bids.push(bid);

                // IMPORTANT: headers NEVER set info.block_hash (not blinded).
                // We only set block_hash from payload ("getPayload") lines.
            }
        }
        "handleGetPayloadV2" | "getPayload"=> {
            if log_entry.message.msg == "received payload from relay" || log_entry.message.msg == "calling getPayload"{
                slot_info.is_payload_received = true;

                // Treat payload blockHash as blinded and add to pending list.
                let bh = log_entry.message.blockHash.clone();
                if !bh.is_empty() && !slot_info.pending_blinded_block_hashes.contains(&bh) {
                    slot_info.pending_blinded_block_hashes.push(bh.clone());
                }

                // Allow payload to set info.block_hash only if currently empty.
                if slot_info.info.block_hash.is_empty() && !bh.is_empty() {
                    slot_info.info.block_hash = bh;
                }

                // Opportunistically capture block number; block_hash stays payload-only.
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

// --------------------------- Reconciliation --------------------------------

pub fn finalize_slot_infos(slot_infos: &mut SlotInfos) {
    // Work slot-by-slot, then apply strictly per-UID.
    let mut slots: Vec<_> = slot_infos.keys().cloned().collect();
    slots.sort();

    for slot in slots {
        let Some(slot_map) = slot_infos.get_mut(&slot) else { continue; };

        // Derive slot start time in RFC3339 (ms, Z) when missing.
        let slot_num_i64 = slot.parse::<i64>().unwrap_or_default();
        let slot_start_dt = get_slot_start_time_utc(slot_num_i64);
        let slot_start_rfc3339 = slot_start_dt.to_rfc3339_opts(SecondsFormat::Millis, true);
        for (_uid, info) in slot_map.iter_mut() {
            if info.time.is_empty() {
                info.time = slot_start_rfc3339.clone();
            }
        }

        #[derive(Clone)]
        struct BidView {
            relay: String,      // owned for safety
            host: String,       // normalized host
            block_hash: String, // blinded-ish (from headers, used only for matching)
            value: Decimal,
        }

        // Collect ALL bids across UIDs in THIS slot (own strings).
        let mut all_bids: Vec<BidView> = Vec::new();
        for (_uid, info) in slot_map.iter() {
            for b in &info.info.bids {
                if b.block_hash.is_empty() { continue; }
                if b.bid_value <= Decimal::ZERO { continue; }
                all_bids.push(BidView {
                    relay: b.relay.clone(),
                    host:  host_from(&b.relay),
                    block_hash: b.block_hash.clone(),
                    value: b.bid_value,
                });
            }
        }

        // If slot has no usable bids, zero-out all UIDs; keep strict hash invariant.
        if all_bids.is_empty() {
            for (_uid, info) in slot_map.iter_mut() {
                if !info.info.block_hash.is_empty()
                    && !info.pending_blinded_block_hashes.contains(&info.info.block_hash)
                {
                    debug!(
                        "[SANITY-EMPTY] Clearing non-blinded block_hash={} (slot {})",
                        info.info.block_hash, slot
                    );
                    info.info.block_hash.clear();
                }

                info.onchain_bid_value = Decimal::ZERO;
                info.is_proxy_win = false;
                info.is_equal_to_proxy_bid = false;
                info.equal_to_proxy_bidders.clear();
                info.el_reward_increase_eth = Decimal::ZERO;
                info.el_reward_increase_wei = U256::zero();
                info.el_reward_increase_percent_precise = Decimal::ZERO;
                info.el_reward_increase_percentage = 0;
                info.second_highest_bid_value = Decimal::ZERO;
                info.second_higher_bid_delivered_relay.clear();
                info.onchain_bid_delivered_relay.clear();
                info.is_winning_bid_highest = false;
                info.fee_per_block = dec!(0.0);
            }
            continue;
        }

        // ----- Per-slot "top" computation -----
        let slot_top_value = all_bids.iter().map(|v| v.value).fold(Decimal::ZERO, Decimal::max);

        let mut rproxy_at_top: Vec<&BidView> = Vec::new();
        let mut nonproxy_at_top_hosts: BTreeSet<String> = BTreeSet::new();
        for v in &all_bids {
            if v.value == slot_top_value {
                if is_relay_proxy(&v.relay) {
                    rproxy_at_top.push(v);
                } else {
                    nonproxy_at_top_hosts.insert(v.host.clone());
                }
            }
        }

        // Best non-proxy strictly below top (uplift base)
        let mut best_nonproxy_val = Decimal::ZERO;
        let mut best_nonproxy_hosts_at_best: BTreeSet<String> = BTreeSet::new();
        for v in &all_bids {
            if !is_relay_proxy(&v.relay) && v.value < slot_top_value {
                match best_nonproxy_val.partial_cmp(&v.value) {
                    Some(std::cmp::Ordering::Less) => {
                        best_nonproxy_val = v.value;
                        best_nonproxy_hosts_at_best.clear();
                        best_nonproxy_hosts_at_best.insert(v.host.clone());
                    }
                    Some(std::cmp::Ordering::Equal) => {
                        best_nonproxy_hosts_at_best.insert(v.host.clone());
                    }
                    _ => {}
                }
            }
        }
        let best_nonproxy_host = best_nonproxy_hosts_at_best
            .iter()
            .next()
            .cloned()
            .unwrap_or_default();

        let slot_is_loss = !nonproxy_at_top_hosts.is_empty();

        // Deterministic proxy candidate when proxies win.
        let (chosen_hash, chosen_proxy_host, uplift, uplift_wei, pct_precise, pct_rounded, fee_per_block) =
            if !slot_is_loss && !rproxy_at_top.is_empty() {
                let uplift = (slot_top_value - best_nonproxy_val).max(Decimal::ZERO);
                let wei_multiplier = Decimal::from(1_000_000_000_000_000_000u128);
                let uplift_wei_dec = (uplift * wei_multiplier).round();
                let uplift_wei =
                    U256::from_dec_str(&uplift_wei_dec.to_string()).unwrap_or_else(|_| U256::zero());
                let pct_precise = if slot_top_value > Decimal::ZERO {
                    (uplift / slot_top_value) * Decimal::from(100)
                } else {
                    Decimal::ZERO
                };
                let pct_rounded = pct_precise.round().to_u64().unwrap_or(0);

                let mut rproxy_sorted = rproxy_at_top.clone();
                rproxy_sorted.sort_by(|a, b| {
                    let o = a.block_hash.cmp(&b.block_hash);
                    if o != std::cmp::Ordering::Equal { return o; }
                    a.host.cmp(&b.host)
                });
                let chosen = rproxy_sorted[0];

                let fee = if pct_precise <= dec!(1) {
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

                (chosen.block_hash.clone(), chosen.host.clone(), uplift, uplift_wei, pct_precise, pct_rounded, fee)
            } else {
                (String::new(), String::new(), Decimal::ZERO, U256::zero(), Decimal::ZERO, 0u64, dec!(0.0))
            };

        // For "loss" reporting
        let eq_nonproxy_join = nonproxy_at_top_hosts.iter().cloned().collect::<Vec<_>>().join(", ");
        let mut top_hosts_all: BTreeSet<String> = BTreeSet::new();
        for v in &all_bids {
            if v.value == slot_top_value {
                top_hosts_all.insert(v.host.clone());
            }
        }
        let top_hosts_join = top_hosts_all.iter().cloned().collect::<Vec<_>>().join(", ");

        // ---- Apply per-UID ONLY IF the UID has a header-bid matching one of its own blinded hashes ----
        for (_uid, info) in slot_map.iter_mut() {
            // Purge any non-blinded hash before writing results
            if !info.info.block_hash.is_empty()
                && !info.pending_blinded_block_hashes.contains(&info.info.block_hash)
            {
                debug!(
                    "[SANITY] Clearing non-blinded block_hash={} (slot {}) before write",
                    info.info.block_hash, slot
                );
                info.info.block_hash.clear();
            }

            let allowed: BTreeSet<&str> = info
                .pending_blinded_block_hashes
                .iter()
                .map(|s| s.as_str())
                .collect();

            let has_header_match = !allowed.is_empty()
                && info.info.bids.iter().any(|b| allowed.contains(b.block_hash.as_str()));

            if !has_header_match {
                // HARD SKIP for this UID
                info.onchain_bid_value = Decimal::ZERO;
                info.is_proxy_win = false;
                info.is_equal_to_proxy_bid = false;
                info.equal_to_proxy_bidders.clear();
                info.el_reward_increase_eth = Decimal::ZERO;
                info.el_reward_increase_wei = U256::zero();
                info.el_reward_increase_percent_precise = Decimal::ZERO;
                info.el_reward_increase_percentage = 0;
                info.second_highest_bid_value = Decimal::ZERO;
                info.second_higher_bid_delivered_relay.clear();
                info.onchain_bid_delivered_relay.clear();
                info.is_winning_bid_highest = false;
                info.fee_per_block = dec!(0.0);
                continue;
            }

            // UID qualifies; apply per-slot classification
            info.onchain_bid_value = slot_top_value;
            info.is_winning_bid_highest = true;

            if slot_is_loss {
                // Non-proxy tied at top → loss/tie scenario
                info.is_proxy_win = false;
                info.is_equal_to_proxy_bid = true;
                info.equal_to_proxy_bidders = eq_nonproxy_join.clone();
                info.onchain_bid_delivered_relay = top_hosts_join.clone();

                info.el_reward_increase_eth = Decimal::ZERO;
                info.el_reward_increase_wei = U256::zero();
                info.el_reward_increase_percent_precise = Decimal::ZERO;
                info.el_reward_increase_percentage = 0;
                info.second_highest_bid_value = Decimal::ZERO;
                info.second_higher_bid_delivered_relay.clear();
                info.fee_per_block = dec!(0.0);
            } else {
                // Proxy wins
                info.is_proxy_win = true;
                info.is_equal_to_proxy_bid = false;
                info.equal_to_proxy_bidders.clear();

                // Only set chosen hash if it's blinded for THIS UID.
                if !chosen_hash.is_empty()
                    && info.pending_blinded_block_hashes.contains(&chosen_hash)
                {
                    info.info.block_hash = chosen_hash.clone();
                    // keep invariant explicit
                    if !info.pending_blinded_block_hashes.contains(&info.info.block_hash) {
                        info.pending_blinded_block_hashes.push(info.info.block_hash.clone());
                    }
                } else if !chosen_hash.is_empty() {
                    debug!(
                        "[STRICT] Chosen hash not in this UID's pending list; not setting block_hash (slot {})",
                        slot
                    );
                }

                info.onchain_bid_delivered_relay = chosen_proxy_host.clone();
                info.second_highest_bid_value = best_nonproxy_val;
                info.second_higher_bid_delivered_relay = best_nonproxy_host.clone();
                info.el_reward_increase_eth = uplift;
                info.el_reward_increase_wei = uplift_wei;
                info.el_reward_increase_percent_precise = pct_precise;
                info.el_reward_increase_percentage = pct_rounded;
                info.fee_per_block = fee_per_block;
            }

            // Final invariant: if a hash is set, it must be blinded (present in pending list).
            if !info.info.block_hash.is_empty()
                && !info.pending_blinded_block_hashes.contains(&info.info.block_hash)
            {
                debug!(
                    "[SANITY-FINAL] Clearing non-blinded block_hash={} (slot {})",
                    info.info.block_hash, slot
                );
                info.info.block_hash.clear();
            }
        }
    }
}

// ------------------------------ Helpers ------------------------------------

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
    serde_json::to_writer_pretty(file, &removed)?;
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
