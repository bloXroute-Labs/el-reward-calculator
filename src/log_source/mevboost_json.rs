use serde_json::{self, Deserializer, Value};
use chrono::{DateTime, SecondsFormat, Utc};
use ethers::types::U256;
use log::debug;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal_macros::dec;
use std::collections::{BTreeSet, HashMap};
use url::Url;

use crate::log_source::common::{get_slot_start_time_utc, is_relay_proxy};
use crate::log_source::types::{Bid, LogEntry, MevBoostRequest, SlotInfo};
use crate::SlotInfos; // <-- SlotInfos is at crate root

/// Parse a stream of MEV-Boost log JSON and populate `slot_infos`.
pub fn parse_file_content<R: std::io::Read>(reader: R, slot_infos: &mut SlotInfos) {
    let stream = Deserializer::from_reader(reader).into_iter::<Value>();
    for entry in stream {
        match entry {
            Ok(Value::Object(map)) => {
                match serde_json::from_value::<LogEntry>(Value::Object(map)) {
                    Ok(log_entry) => process_json(&log_entry, slot_infos),
                    Err(e) => eprintln!("Failed to parse log entry: {}. Skipping.", e),
                }
            }
            Ok(Value::Array(vec)) => {
                for item in vec {
                    match serde_json::from_value::<LogEntry>(item) {
                        Ok(log_entry) => process_json(&log_entry, slot_infos),
                        Err(e) => eprintln!("Failed to parse log entry: {}. Skipping.", e),
                    }
                }
            }
            _ => { /* ignore non-object JSON */ }
        }
    }

    // Compute per-UID classification, strictly per-request (mirrors Commit-Boost pass logic).
    post_process_all_slots(slot_infos);
}

// ----------------------------- Parsing -------------------------------------

fn process_json(log_entry: &LogEntry, slot_infos: &mut SlotInfos) {
    let slot = log_entry.message.slot.clone();
    let slot_uid = log_entry.message.slotUID.clone(); // treat slotUID as req_id

    let slot_info_map = slot_infos.entry(slot.clone()).or_insert_with(HashMap::new);

    // Ensure merging happens if slot_uid already exists (payload-only block_hash invariant)
    let slot_info = slot_info_map
        .entry(slot_uid.clone())
        .and_modify(|existing| merge_fields_from_log_entry(existing, log_entry))
        .or_insert_with(|| make_slot_info_with_slot_uid_and_slot(slot_uid.clone(), slot.clone()));

    match log_entry.message.method.as_str() {
        // ------------------------ HEADERS ------------------------
        // NOTE: We DO NOT set info.block_hash from headers; only payload (blinded) sets it.
        "getHeader" => {
            if log_entry.message.msg == "bid received" {
                let req_id = if !slot_uid.is_empty() { slot_uid.clone() } else { "default".to_string() };

                // Build Bid
                let mut bid: Bid = Default::default();
                if let Ok(date) = DateTime::parse_from_rfc3339(&log_entry.message.time) {
                    bid.timestamp = date.with_timezone(&Utc).timestamp();
                }

                bid.slot         = log_entry.message.slot.clone();
                bid.block_hash   = log_entry.message.blockHash.clone(); // used for matching to payload hash
                bid.parent_hash  = log_entry.message.parentHash.clone();
                bid.ua           = log_entry.message.ua.clone();
                bid.relay        = log_entry.message.url.as_deref().unwrap_or("").to_string();
                bid.pubkey       = log_entry.message.pubkey.as_deref().unwrap_or("").to_string();
                bid.block_number = log_entry.message.blockNumber.map_or(String::new(), |n| n.to_string());
                bid.bid_value    = log_entry.message.value.as_deref()
                    .unwrap_or("0.0")
                    .parse::<Decimal>()
                    .unwrap_or(Decimal::ZERO);

                slot_info
                    .requests
                    .entry(req_id.clone())
                    .or_insert_with(Default::default)
                    .bids
                    .push(bid.clone());

                // Late header ↔ payload match: if header hash equals a pending blinded payload hash, select this req.
                if slot_info.selected_req_id.is_none()
                    && !bid.block_hash.is_empty()
                    && slot_info.pending_blinded_block_hashes.contains(&bid.block_hash)
                {
                    debug!(
                        "[RESOLVE/HEADER] Matched payload hash {}; selected_req_id={}",
                        bid.block_hash, req_id
                    );
                    slot_info.selected_req_id = Some(req_id);
                    // Keep payload-only invariant: we DO NOT set info.block_hash here.
                }
            }
        }

        // ------------------------ PAYLOADS ------------------------
        "getPayload" => {
            if log_entry.message.msg == "received payload from relay" {
                slot_info.is_payload_received = true;

                // Treat payload blockHash as "blinded": push to pending list; set info.block_hash if empty.
                let block_hash = log_entry.message.blockHash.clone();
                if !block_hash.is_empty() && !slot_info.pending_blinded_block_hashes.contains(&block_hash) {
                    slot_info.pending_blinded_block_hashes.push(block_hash.clone());
                }
                if slot_info.info.block_hash.is_empty() && !block_hash.is_empty() {
                    slot_info.info.block_hash = block_hash.clone();
                }

                // Opportunistically capture block_number
                merge_fields_from_log_entry(slot_info, log_entry);

                // Try to match payload hash to an existing request; choose best request by
                // highest bid FOR this block_hash (ties by req_id lexicographically).
                if !block_hash.is_empty() {
                    let mut matched: Vec<(&String, &MevBoostRequest)> = slot_info
                        .requests
                        .iter()
                        .filter(|(_, req)| req.bids.iter().any(|b| b.block_hash == block_hash))
                        .collect();

                    if !matched.is_empty() {
                        matched.sort_by(|(aid, a), (bid, b)| {
                            let a_max = a.bids.iter()
                                .filter(|b| b.block_hash == block_hash)
                                .map(|b| b.bid_value)
                                .fold(Decimal::ZERO, Decimal::max);
                            let b_max = b.bids.iter()
                                .filter(|b| b.block_hash == block_hash)
                                .map(|b| b.bid_value)
                                .fold(Decimal::ZERO, Decimal::max);

                            b_max.cmp(&a_max).then_with(|| aid.cmp(bid))
                        });

                        let (best_req_id, _) = matched[0];
                        if slot_info.selected_req_id.is_none() || slot_info.info.block_hash != block_hash {
                            debug!(
                                "[RESOLVE/PAYLOAD] payload matched; selected_req_id={} block_hash={}",
                                best_req_id, block_hash
                            );
                            slot_info.selected_req_id = Some(best_req_id.clone());
                        }
                    } else if slot_info.selected_req_id.is_none() && slot_info.requests.len() == 1 {
                        // No per-hash match but only one request exists → select it
                        let only = slot_info.requests.keys().next().unwrap().clone();
                        debug!("[RESOLVE/PAYLOAD/ONE] only one request; selected_req_id={}", only);
                        slot_info.selected_req_id = Some(only);
                    }
                } else if slot_info.selected_req_id.is_none() && slot_info.requests.len() == 1 {
                    let only = slot_info.requests.keys().next().unwrap().clone();
                    debug!("[RESOLVE/PAYLOAD/ONE-NOHASH] only one request; selected_req_id={}", only);
                    slot_info.selected_req_id = Some(only);
                }
            }
        }
        _ => {}
    }
}

// --------------------------- Reconciliation --------------------------------

pub fn post_process_all_slots(slot_infos: &mut SlotInfos) {
    let mut slots: Vec<_> = slot_infos.keys().cloned().collect();
    slots.sort();

    // ---------- Pass 1: late match + strict fallback + invariants ----------
    for slot in &slots {
        if let Some(slot_map) = slot_infos.get_mut(slot) {
            let mut slot_uids: Vec<_> = slot_map.keys().cloned().collect();
            slot_uids.sort();

            for slot_uid in slot_uids {
                if let Some(info) = slot_map.get_mut(&slot_uid) {
                    // If nothing selected yet and there is exactly one request, select it.
                    if info.selected_req_id.is_none() && info.requests.len() == 1 {
                        let only = info.requests.keys().next().cloned().unwrap();
                        debug!("[AUTO-SELECT/ONE] uid={} selected_req_id={}", slot_uid, only);
                        info.selected_req_id = Some(only);
                    }

                    if info.selected_req_id.is_none() && !info.pending_blinded_block_hashes.is_empty() {
                        let allowed: BTreeSet<&str> = info
                            .pending_blinded_block_hashes
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<BTreeSet<&str>>(); // explicit type to help inference

                        // Choose best request by the highest bid that matches any blinded hash
                        let mut best_allowed: Option<(String, String, Decimal)> = None; // (req_id, block_hash, val)
                        for (rid, req) in &info.requests {
                            for b in &req.bids {
                                if b.bid_value <= Decimal::ZERO { continue; }
                                if b.block_hash.is_empty() { continue; }
                                if !allowed.contains(b.block_hash.as_str()) { continue; }
                                match best_allowed {
                                    Some((_, _, cur)) if b.bid_value <= cur => {}
                                    _ => best_allowed = Some((rid.clone(), b.block_hash.clone(), b.bid_value)),
                                }
                            }
                        }

                        if let Some((best_req_id, block_hash, _)) = best_allowed {
                            debug!("[AUTO-MATCH/STRICT] req_id={} block_hash={}", best_req_id, block_hash);
                            info.selected_req_id = Some(best_req_id);
                            // Keep payload-only invariant: info.block_hash may already be payload-set.
                            if info.info.block_hash.is_empty() {
                                info.info.block_hash = block_hash;
                            }
                        }
                    }

                    // Hash invariant: if block_hash set, it MUST be in pending_blinded_block_hashes
                    if !info.info.block_hash.is_empty()
                        && !info.pending_blinded_block_hashes.contains(&info.info.block_hash)
                    {
                        debug!(
                            "[SANITY] Clearing non-blinded block_hash={} (uid={})",
                            info.info.block_hash, slot_uid
                        );
                        info.info.block_hash.clear();
                    }
                }
            }
        }
    }

    // ---------- Pass 2: PER-UID, PER-REQUEST reconciliation ----------
    for slot in slots {
        let Some(slot_map) = slot_infos.get_mut(&slot) else { continue; };

        // Set RFC3339 slot-start time (once per UID)
        let slot_i64 = slot.parse::<i64>().unwrap_or_default();
        let slot_start_dt = get_slot_start_time_utc(slot_i64);
        let slot_start_rfc3339 = slot_start_dt.to_rfc3339_opts(SecondsFormat::Millis, true);
        for (_uid, info) in slot_map.iter_mut() {
            if info.time.is_empty() {
                info.time = slot_start_rfc3339.clone();
            }
        }

        for (_uid, info) in slot_map.iter_mut() {
            // Require a selected request; if none, try a sensible fallback:
            let selected_req = if let Some(rid) = info.selected_req_id.as_ref() {
                info.requests.get(rid)
            } else {
                // Fallback: best request by max relay-proxy bid that matches blinded set (if present),
                // else best by max relay-proxy bid overall.
                let allowed_opt: Option<BTreeSet<&str>> = if info.pending_blinded_block_hashes.is_empty() {
                    None
                } else {
                    Some(
                        info.pending_blinded_block_hashes
                            .iter()
                            .map(|s| s.as_str())
                            .collect::<BTreeSet<&str>>(), // explicit
                    )
                };

                let mut best: Option<(&String, &MevBoostRequest, Decimal)> = None;

                for (rid, req) in &info.requests {
                    let mut best_in_req = Decimal::ZERO;
                    for b in &req.bids {
                        if b.bid_value <= Decimal::ZERO { continue; }
                        if !is_relay_proxy(&b.relay) { continue; }
                        if let Some(ref allow_set) = allowed_opt {
                            if !allow_set.contains(b.block_hash.as_str()) { continue; }
                        }
                        if b.bid_value > best_in_req {
                            best_in_req = b.bid_value;
                        }
                    }
                    if best_in_req > Decimal::ZERO {
                        match best {
                            Some((_, _, cur)) if best_in_req <= cur => {}
                            _ => best = Some((rid, req, best_in_req)),
                        }
                    }
                }

                if let Some((rid, _req, _)) = best {
                    info.selected_req_id = Some(rid.clone());
                    info.requests.get(rid)
                } else {
                    None
                }
            };

            // If we still have no request, or no blinded/header match is possible → zero the metrics & continue.
            let Some(req) = selected_req else {
                zero_out(info);
                continue;
            };

            // Build request-scoped bids (>0 only)
            let req_bids: Vec<&Bid> = req.bids.iter().filter(|b| b.bid_value > Decimal::ZERO).collect();
            if req_bids.is_empty() {
                zero_out(info);
                continue;
            }

            // Determine/validate chosen blinded hash within THIS request only.
            let allowed_set: BTreeSet<&str> = info
                .pending_blinded_block_hashes
                .iter()
                .map(|s| s.as_str())
                .collect::<BTreeSet<&str>>(); // explicit

            // If block_hash already set but not in allowed → clear it.
            if !info.info.block_hash.is_empty() && !allowed_set.contains(info.info.block_hash.as_str()) {
                debug!("[SANITY-REQ] Clearing non-blinded block_hash={} (slot={})", info.info.block_hash, slot);
                info.info.block_hash.clear();
            }

            // If block_hash empty, pick best hash inside this request that’s in the blinded set.
            if info.info.block_hash.is_empty() {
                let mut best_for_allowed: Option<(String, Decimal)> = None;
                for b in &req_bids {
                    if b.block_hash.is_empty() || !allowed_set.contains(b.block_hash.as_str()) { continue; }
                    match best_for_allowed {
                        Some((_, ref cur)) if b.bid_value <= *cur => {}
                        _ => best_for_allowed = Some((b.block_hash.clone(), b.bid_value)),
                    }
                }
                if let Some((hash, _)) = best_for_allowed {
                    info.info.block_hash = hash;
                } else {
                    zero_out(info);
                    continue;
                }
            }

            // Keep the chosen blinded hash in pending list
            if !info.info.block_hash.is_empty()
                && !info.pending_blinded_block_hashes.contains(&info.info.block_hash)
            {
                info.pending_blinded_block_hashes.push(info.info.block_hash.clone());
            }

            // Request-scope maxima & classification
            let req_top_value = req_bids.iter().map(|b| b.bid_value).max().unwrap_or(Decimal::ZERO);

            // Partition tops inside this request
            let mut rproxy_at_top_hosts: BTreeSet<String> = BTreeSet::new();
            let mut nonproxy_at_top_hosts: BTreeSet<String> = BTreeSet::new();
            for b in &req_bids {
                if b.bid_value == req_top_value {
                    let h = host_from(&b.relay);
                    if is_relay_proxy(&b.relay) { rproxy_at_top_hosts.insert(h); }
                    else { nonproxy_at_top_hosts.insert(h); }
                }
            }

            // Winner for the *chosen hash* (deterministic host selection)
            let mut candidates_for_hash: Vec<&Bid> = req_bids
                .iter()
                .copied()
                .filter(|b| b.block_hash == info.info.block_hash)
                .collect();

            // If no candidates for the chosen hash, treat as no-header-match (safety)
            if candidates_for_hash.is_empty() {
                zero_out(info);
                continue;
            }

            candidates_for_hash.sort_by(|a, b| {
                let o = b.bid_value.cmp(&a.bid_value);
                if o != std::cmp::Ordering::Equal { return o; }
                host_from(&a.relay).cmp(&host_from(&b.relay))
            });
            let winner = candidates_for_hash[0];
            let onchain_val = winner.bid_value;

            // Is there a non-proxy tie at the very top (within this request)?
            let req_is_loss = !nonproxy_at_top_hosts.is_empty();

            // Best non-proxy below winner (within request)
            let second_best_nonproxy = req_bids
                .iter()
                .copied()
                .filter(|b| !is_relay_proxy(&b.relay) && b.bid_value < onchain_val)
                .max_by(|a, b| a.bid_value.cmp(&b.bid_value));

            let second_val = second_best_nonproxy.map(|b| b.bid_value).unwrap_or(Decimal::ZERO);
            let second_host = second_best_nonproxy.map(|b| host_from(&b.relay)).unwrap_or_default();

            // Fill fields
            info.onchain_bid_value = onchain_val;
            info.is_winning_bid_highest = onchain_val == req_top_value;

            if req_is_loss {
                info.is_proxy_win = false;
                info.is_equal_to_proxy_bid = true;
                info.equal_to_proxy_bidders = nonproxy_at_top_hosts.iter().cloned().collect::<Vec<_>>().join(", ");

                // Delivered host: show all top hosts in this request
                let mut all_top: BTreeSet<String> = BTreeSet::new();
                for b in &req_bids {
                    if b.bid_value == req_top_value {
                        all_top.insert(host_from(&b.relay));
                    }
                }
                info.onchain_bid_delivered_relay = all_top.into_iter().collect::<Vec<_>>().join(", ");

                // Zero uplift/fee on loss/tie
                info.second_highest_bid_value = Decimal::ZERO;
                info.second_higher_bid_delivered_relay.clear();
                info.el_reward_increase_wei = U256::zero();
                info.el_reward_increase_eth = Decimal::ZERO;
                info.el_reward_increase_percent_precise = Decimal::ZERO;
                info.el_reward_increase_percentage = 0;
                info.fee_per_block = dec!(0.0);
            } else {
                // Proxy win within this request
                info.is_proxy_win = true;
                info.is_equal_to_proxy_bid = false;
                info.equal_to_proxy_bidders.clear();

                // Delivered host for the chosen hash @ winner value (lexicographically smallest)
                let mut hosts_for_val: Vec<String> = candidates_for_hash
                    .iter()
                    .filter(|b| b.bid_value == onchain_val)
                    .map(|b| host_from(&b.relay))
                    .collect();
                hosts_for_val.sort();
                info.onchain_bid_delivered_relay =
                    hosts_for_val.get(0).cloned().unwrap_or_else(|| host_from(&winner.relay));

                info.second_highest_bid_value = second_val;
                info.second_higher_bid_delivered_relay = second_host;

                // Non-negative uplift & fees (request scoped)
                let mut uplift = onchain_val - second_val;
                if uplift.is_sign_negative() { uplift = Decimal::ZERO; }

                if uplift.is_zero() || onchain_val.is_zero() {
                    info.el_reward_increase_wei = U256::zero();
                    info.el_reward_increase_eth = Decimal::ZERO;
                    info.el_reward_increase_percent_precise = Decimal::ZERO;
                    info.el_reward_increase_percentage = 0;
                    info.fee_per_block = dec!(0.0);
                } else {
                    let wei_multiplier = Decimal::from(1_000_000_000_000_000_000u128);
                    let uplift_wei_dec = (uplift * wei_multiplier).round();
                    let uplift_wei = U256::from_dec_str(&uplift_wei_dec.to_string()).unwrap_or_else(|_| U256::zero());
                    let pct_precise = (uplift / onchain_val) * Decimal::from(100);

                    info.el_reward_increase_wei = uplift_wei;
                    info.el_reward_increase_eth = uplift;
                    info.el_reward_increase_percent_precise = pct_precise;
                    info.el_reward_increase_percentage = pct_precise.round().to_u64().unwrap_or(0);

                    info.fee_per_block = if pct_precise <= dec!(1) {
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

fn host_from(relay: &str) -> String {
    Url::parse(relay)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_else(|| relay.to_string())
}

fn zero_out(info: &mut SlotInfo) {
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

/// Private helper to create SlotInfo without redefining inherent methods (avoid E0592).
fn make_slot_info_with_slot_uid_and_slot(slot_uid: String, slot: String) -> SlotInfo {
    let mut s = SlotInfo::new(slot_uid);
    s.slot = slot;
    s
}

/// Private helper (instead of an inherent method) to merge payload-only fields
/// so we don't collide with any impl in your types.
fn merge_fields_from_log_entry(info: &mut SlotInfo, log_entry: &LogEntry) {
    if log_entry.message.method == "getPayload"
        && log_entry.message.msg == "received payload from relay"
        && !log_entry.message.blockHash.is_empty()
    {
        let bh = log_entry.message.blockHash.clone();

        if !info.pending_blinded_block_hashes.contains(&bh) {
            info.pending_blinded_block_hashes.push(bh.clone());
        }
        if info.info.block_hash.is_empty() {
            info.info.block_hash = bh;
        }
    }

    if info.block_number.is_empty() {
        if let Some(num) = log_entry.message.blockNumber {
            if num != 0 {
                info.block_number = num.to_string();
            }
        }
    }
}
