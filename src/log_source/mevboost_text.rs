use lazy_static::lazy_static;
use regex::Regex;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use rust_decimal::prelude::ToPrimitive;
use crate::{SlotInfo, SlotInfos};
use chrono::{DateTime, Utc};
use std::collections::{HashMap, BTreeSet};
use crate::Bid;
use url::Url;
use ethers::types::U256;
use crate::log_source::common::{is_relay_proxy};
use log::debug;

// ===============================
// Commit-Boost text regex patterns
// (kept because your parser handles both sources)
// ===============================
lazy_static! {
    pub static ref GETHEADER_REQ_START: Regex =
        Regex::new(r"getHeader request start.*?msIntoSlot=(\d+).*?slot=(\d+).*?slotUID=([\w\-]+)")
            .unwrap();
    pub static ref BID_RECEIVED: Regex =
        Regex::new(r#"msg=\\?\"bid received\\?\"(?:\s+\S+)*\s+slot=(\d+)\s+slotUID=([\w\-]+)"#).unwrap();
    pub static ref GETPAYLOAD_REQ_START: Regex = Regex::new(
        r"submitBlindedBlock request start.*?msIntoSlot=(\d+).*?slot=(\d+).*?slotUID=([\w\-]+)"
    ).unwrap();
    pub static ref PAYLOAD_RECEIVED: Regex =
        Regex::new(r"received payload from relay.*?slot=(\d+).*?slotUID=([\w\-]+)").unwrap();
}

// ===============================
// MEV-Boost text regex patterns
// ===============================
lazy_static! {
    // msg="getHeader request start - 253 milliseconds into slot 12216021"
    pub static ref MEV_GETHEADER_REQ_START_A: Regex = Regex::new(
        r#"msg=\\?"getHeader request start\s*-\s*(\d+)\s*milliseconds into slot\s*(\d+)\\?""#
    ).unwrap();
    // alt form with kvs
    pub static ref MEV_GETHEADER_REQ_START_B: Regex = Regex::new(
        r#"getHeader request start.*?msIntoSlot=(\d+).*?slot=(\d+)"#
    ).unwrap();

    pub static ref MEV_BID_RECEIVED: Regex = Regex::new(
        r#"msg=\\?"bid received\\?""#
    ).unwrap();

    pub static ref MEV_BEST_BID: Regex = Regex::new(
        r#"msg=\\?"best bid\\?""#
    ).unwrap();

    pub static ref MEV_GETPAYLOAD_REQ_START: Regex = Regex::new(
        r#"msg=\\?"getPayload request start\s*-\s*(\d+)\s*milliseconds into slot\s*(\d+)\\?""#
    ).unwrap();

    pub static ref MEV_PAYLOAD_RECEIVED: Regex = Regex::new(
        r#"msg=\\?"received payload from relay\\?""#
    ).unwrap();
}

// --------- small helpers ---------
fn get_kv<'a>(line: &'a str, key: &str) -> Option<String> {
    // crude kv parser: key=value or key="value"
    for part in line.split_whitespace() {
        if let Some((k, v)) = part.split_once('=') {
            if k.trim() == key {
                return Some(v.trim_matches('"').to_string());
            }
        }
    }
    None
}

fn get_slot_uid_or(slot: &str, line: &str) -> String {
    // If a slotUID is present, use it; else fall back to {slot}_{parentHash} if present; else slot
    if let Some(uid) = get_kv(line, "slotUID") {
        if !uid.is_empty() {
            return uid;
        }
    }
    if let Some(ph) = get_kv(line, "parentHash") {
        if !ph.is_empty() {
            return format!("{}_{}", slot, ph);
        }
    }
    slot.to_string()
}

fn host_from_str(relay: &str) -> String {
    Url::parse(relay)
        .ok()
        .and_then(|u| u.host_str().map(|s| s.to_string()))
        .unwrap_or_else(|| relay.to_string())
}

// ===============================
// Commit-Boost: pass 1 (text)
// (unchanged structurally, but keep the same invariants: payload adds to pending, header only resolves)
// ===============================
pub fn process_lines_first_pass(line: String, slot_infos: &mut SlotInfos) {
    if let Some(captures) = GETHEADER_REQ_START.captures(&line) {
        let ms_into_slot = captures[1].parse::<i64>().unwrap_or(0);
        let slot = &captures[2];
        let slot_uid = &captures[3];
        debug!("[GETHEADER] slot: {}, slot_uid: {}, ms_into_slot: {}.", slot, slot_uid, ms_into_slot);

        let slot_info_with_uid = slot_infos.entry(slot.to_string()).or_insert_with(HashMap::new);
        let slot_info = slot_info_with_uid.entry(slot_uid.to_string()).or_insert_with(|| SlotInfo::new(slot_uid.to_string()));
        slot_info.info.header_start_ms_into_slot = ms_into_slot;
        slot_info.slot = slot.to_string();

    } else if let Some(captures) = BID_RECEIVED.captures(&line) {
        let slot = &captures[1];
        let slot_uid = &captures[2];

        let slot_info_with_uid = slot_infos.entry(slot.to_string()).or_insert_with(HashMap::new);
        let slot_info = slot_info_with_uid.entry(slot_uid.to_string()).or_insert_with(|| SlotInfo::new(slot_uid.to_string()));

        let mut bid: Bid = Default::default();
        bid.slot = slot.to_string();

        // Parse kvs
        for part in line.split_whitespace() {
            if let Some((key, value)) = part.split_once('=') {
                let key = key.trim();
                let value = value.trim_matches('"');
                match key {
                    "time" => {
                        if let Ok(date) = DateTime::parse_from_rfc3339(value) {
                            bid.timestamp = date.with_timezone(&Utc).timestamp();
                        }
                    }
                    "blockHash"   => bid.block_hash = value.to_string(),
                    "parentHash"  => bid.parent_hash = value.to_string(),
                    "pubkey"      => bid.pubkey = value.to_string(),
                    "blockNumber" => bid.block_number = value.to_string(),
                    "ua"          => bid.ua = value.to_string(),
                    "value"       => bid.bid_value = value.parse::<Decimal>().unwrap_or_default(),
                    "url" => {
                        bid.relay = Url::parse(value)
                            .ok()
                            .and_then(|url| url.domain().map(String::from))
                            .unwrap_or_else(|| value.to_string());
                    }
                    _ => {}
                }
            }
        }

        let new_bid_hash = bid.block_hash.clone();
        let new_bid_block_number = bid.block_number.clone();
        slot_info.info.bids.push(bid);

        // Resolve ONLY if pending contains this blinded hash; do NOT remove from pending
        if !new_bid_hash.is_empty()
            && slot_info.info.block_hash.is_empty()
            && slot_info.pending_blinded_block_hashes.contains(&new_bid_hash)
        {
            debug!(
                "[RESOLVE] Found pending blinded hash {} via header; locking block_hash (slot_uid={})",
                new_bid_hash, slot_uid
            );
            slot_info.info.block_hash = new_bid_hash.clone();
            if !new_bid_block_number.is_empty() && slot_info.block_number.is_empty() {
                slot_info.block_number = new_bid_block_number;
            }
        }

    } else if let Some(captures) = GETPAYLOAD_REQ_START.captures(&line) {
        let ms_into_slot = captures[1].parse::<i64>().unwrap_or(0);
        let slot = &captures[2];
        let slot_uid = &captures[3];

        let slot_info_with_uid = slot_infos.entry(slot.to_string()).or_insert_with(HashMap::new);
        let slot_info = slot_info_with_uid.entry(slot_uid.to_string()).or_insert_with(|| SlotInfo::new(slot_uid.to_string()));
        slot_info.info.payload_start_ms_into_slot = ms_into_slot;

        // Only payload may add to pending_blinded_block_hashes
        if let Some(ph) = get_kv(&line, "blockHash") {
            if !ph.is_empty() && !slot_info.pending_blinded_block_hashes.contains(&ph) {
                debug!("[DEFER] payload blinded hash {}; storing pending (slot_uid={})", ph, slot_uid);
                slot_info.pending_blinded_block_hashes.push(ph);
            }
        }

    } else if let Some(captures) = PAYLOAD_RECEIVED.captures(&line) {
        let slot = &captures[1];
        let slot_uid = &captures[2];
        let slot_info_with_uid = slot_infos.entry(slot.to_string()).or_insert_with(HashMap::new);
        let slot_info = slot_info_with_uid.entry(slot_uid.to_string()).or_insert_with(|| SlotInfo::new(slot_uid.to_string()));
        slot_info.is_payload_received = true;
    }
}

// ===============================
// MEV-Boost: pass 1 (text)
// Enforces the same invariants as above.
// ===============================
pub fn process_lines_first_pass_mev(line: String, slot_infos: &mut SlotInfos) {
    // getHeader request start (two forms)
    if let Some(caps) = MEV_GETHEADER_REQ_START_A.captures(&line) {
        let ms = caps[1].parse::<i64>().unwrap_or(0);
        let slot = &caps[2];
        let slot_uid = get_slot_uid_or(slot, &line);

        let slot_map = slot_infos.entry(slot.to_string()).or_insert_with(HashMap::new);
        let slot_info = slot_map.entry(slot_uid.clone()).or_insert_with(|| SlotInfo::new(slot_uid));
        slot_info.info.header_start_ms_into_slot = ms;
        slot_info.slot = slot.to_string();
        return;
    }
    if let Some(caps) = MEV_GETHEADER_REQ_START_B.captures(&line) {
        let ms = caps[1].parse::<i64>().unwrap_or(0);
        let slot = &caps[2];
        let slot_uid = get_slot_uid_or(slot, &line);

        let slot_map = slot_infos.entry(slot.to_string()).or_insert_with(HashMap::new);
        let slot_info = slot_map.entry(slot_uid.clone()).or_insert_with(|| SlotInfo::new(slot_uid));
        slot_info.info.header_start_ms_into_slot = ms;
        slot_info.slot = slot.to_string();
        return;
    }

    // Bid lines – ("bid received" or "best bid")
    if MEV_BID_RECEIVED.is_match(&line) || MEV_BEST_BID.is_match(&line) {
        let Some(slot) = get_kv(&line, "slot") else { return; };
        let slot_uid = get_slot_uid_or(&slot, &line);

        let slot_map = slot_infos.entry(slot.clone()).or_insert_with(HashMap::new);
        let slot_info = slot_map.entry(slot_uid.clone()).or_insert_with(|| SlotInfo::new(slot_uid.clone()));

        let mut bid: Bid = Default::default();
        bid.slot = slot.clone();

        for part in line.split_whitespace() {
            if let Some((key, value)) = part.split_once('=') {
                let key = key.trim();
                let value = value.trim_matches('"');
                match key {
                    "time" => {
                        if let Ok(date) = DateTime::parse_from_rfc3339(value) {
                            bid.timestamp = date.with_timezone(&Utc).timestamp();
                        }
                    }
                    "blockHash"   => bid.block_hash = value.to_string(),
                    "parentHash"  => bid.parent_hash = value.to_string(),
                    "pubkey"      => bid.pubkey = value.to_string(),
                    "blockNumber" => bid.block_number = value.to_string(),
                    "ua"          => bid.ua = value.to_string(),
                    "value" | "blockValue" => {
                        bid.bid_value = value.parse::<Decimal>().unwrap_or_default();
                    }
                    "url" | "relay" => {
                        bid.relay = Url::parse(value)
                            .ok()
                            .and_then(|u| u.domain().map(String::from))
                            .unwrap_or_else(|| value.to_string());
                    }
                    _ => {}
                }
            }
        }

        let new_bid_hash = bid.block_hash.clone();
        let new_bid_block_number = bid.block_number.clone();
        slot_info.info.bids.push(bid);

        // Only resolve if this bid matches a pending blinded hash; keep chosen hash IN pending
        if !new_bid_hash.is_empty()
            && slot_info.info.block_hash.is_empty()
            && slot_info.pending_blinded_block_hashes.contains(&new_bid_hash)
        {
            debug!(
                "[MEV RESOLVE] Found pending blinded hash {} via header; locking block_hash (slot_uid={})",
                new_bid_hash, slot_uid
            );
            slot_info.info.block_hash = new_bid_hash.clone();
            if !new_bid_block_number.is_empty() && slot_info.block_number.is_empty() {
                slot_info.block_number = new_bid_block_number;
            }
        }
        return;
    }

    // getPayload request start (MEV) — only source of pending blinded hashes
    if let Some(caps) = MEV_GETPAYLOAD_REQ_START.captures(&line) {
        let ms = caps[1].parse::<i64>().unwrap_or(0);
        let slot = &caps[2];
        let slot_uid = get_slot_uid_or(slot, &line);

        let slot_map = slot_infos.entry(slot.to_string()).or_insert_with(HashMap::new);
        let slot_info = slot_map.entry(slot_uid.clone()).or_insert_with(|| SlotInfo::new(slot_uid));
        slot_info.info.payload_start_ms_into_slot = ms;

        if let Some(ph) = get_kv(&line, "blockHash") {
            if !ph.is_empty() && !slot_info.pending_blinded_block_hashes.contains(&ph) {
                debug!("[MEV DEFER] payload blinded hash {}; storing pending", ph);
                slot_info.pending_blinded_block_hashes.push(ph);
            }
        }
        return;
    }

    // Payload received (MEV)
    if MEV_PAYLOAD_RECEIVED.is_match(&line) {
        if let Some(slot) = get_kv(&line, "slot") {
            let slot_uid = get_slot_uid_or(&slot, &line);
            let slot_map = slot_infos.entry(slot.to_string()).or_insert_with(HashMap::new);
            let slot_info = slot_map.entry(slot_uid).or_insert_with(|| SlotInfo::new(format!("slot_only_{}", slot)));
            slot_info.is_payload_received = true;
        }
        return;
    }
}

// ===============================
// Finalization (shared by both)
// Strict blinded-only logic + header requirement
// ===============================
pub fn finalize_slot_infos(slot_infos: &mut SlotInfos) {
    for (_slot, slot_map) in slot_infos.iter_mut() {
        for (slot_uid, slot_info) in slot_map.iter_mut() {
            // Sort bids descending by value
            slot_info.info.bids.sort_by(|a, b| b.bid_value.cmp(&a.bid_value));

            // 0) Clear any non-blinded leftovers before use
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

            // 1) Choose a blinded hash that also appears in header bids.
            //    Prefer the already-set one; else best among pending by max bid.
            let chosen_hash = if !slot_info.info.block_hash.is_empty()
                && slot_info
                    .info
                    .bids
                    .iter()
                    .any(|b| b.block_hash == slot_info.info.block_hash)
            {
                slot_info.info.block_hash.clone()
            } else {
                let mut best: Option<(String, Decimal)> = None;
                for ph in &slot_info.pending_blinded_block_hashes {
                    if ph.is_empty() { continue; }
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
                best.map(|(h, _)| h).unwrap_or_default()
            };

            // 2) Must have a chosen blinded hash AND at least one header bid using it
            if chosen_hash.is_empty()
                || !slot_info.info.bids.iter().any(|b| b.block_hash == chosen_hash)
            {
                debug!(
                    "[SKIP] No header matched any blinded payload hash (uid={}). No EL calc.",
                    slot_uid
                );
                // zero out metrics
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

            // 3) Enforce invariant: chosen hash must be in pending_blinded_block_hashes
            if !slot_info
                .pending_blinded_block_hashes
                .contains(&chosen_hash)
            {
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

            // 4) Lock the chosen blinded hash (ensure pending includes it)
            slot_info.info.block_hash = chosen_hash.clone();
            if !slot_info
                .pending_blinded_block_hashes
                .contains(&slot_info.info.block_hash)
            {
                slot_info
                    .pending_blinded_block_hashes
                    .push(slot_info.info.block_hash.clone());
            }

            // Candidate header bids for this hash (sort by value desc, then host asc)
            let mut candidates: Vec<&Bid> = slot_info
                .info
                .bids
                .iter()
                .filter(|b| b.block_hash == slot_info.info.block_hash)
                .collect();
            if candidates.is_empty() {
                // Safety net (shouldn’t happen due to check above)
                continue;
            }
            candidates.sort_by(|a, b| {
                let o = b.bid_value.cmp(&a.bid_value);
                if o != std::cmp::Ordering::Equal { return o; }
                host_from_str(&a.relay).cmp(&host_from_str(&b.relay))
            });

            let winner_bid = candidates[0];
            let onchain_val = winner_bid.bid_value;

            // Highest value across slot, to check top/ties
            let slot_top_value = slot_info
                .info
                .bids
                .iter()
                .map(|b| b.bid_value)
                .max()
                .unwrap_or(Decimal::ZERO);

            // Detect top non-proxy ties
            let top_nonproxy_hosts: BTreeSet<String> = slot_info
                .info
                .bids
                .iter()
                .filter(|b| b.bid_value == slot_top_value && !is_relay_proxy(&b.relay))
                .map(|b| host_from_str(&b.relay))
                .collect();
            let top_proxy_exists = slot_info
                .info
                .bids
                .iter()
                .any(|b| b.bid_value == slot_top_value && is_relay_proxy(&b.relay));

            let chosen_is_proxy = is_relay_proxy(&winner_bid.relay);
            let is_equal_to_proxy_bid = !top_nonproxy_hosts.is_empty() && top_proxy_exists;
            let is_proxy_win = chosen_is_proxy && (slot_top_value == onchain_val) && !is_equal_to_proxy_bid;

            // Delivered host for chosen hash at chosen value (lexicographically smallest)
            let mut chosen_hosts_for_hash: Vec<String> = candidates
                .iter()
                .filter(|b| b.bid_value == onchain_val)
                .map(|b| host_from_str(&b.relay))
                .collect();
            chosen_hosts_for_hash.sort();
            let delivered_host = chosen_hosts_for_hash
                .get(0)
                .cloned()
                .unwrap_or_else(|| host_from_str(&winner_bid.relay));

            // Best non-proxy < onchain value
            let second_best_nonproxy = slot_info
                .info
                .bids
                .iter()
                .filter(|b| !is_relay_proxy(&b.relay) && b.bid_value < onchain_val)
                .max_by(|a, b| a.bid_value.cmp(&b.bid_value));

            let second_best_val = second_best_nonproxy.map(|b| b.bid_value).unwrap_or(Decimal::ZERO);
            let second_best_host = second_best_nonproxy
                .map(|b| host_from_str(&b.relay))
                .unwrap_or_default();

            // Populate fields
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

                // Non-negative uplift
                let mut uplift = onchain_val - second_best_val;
                if uplift.is_sign_negative() { uplift = Decimal::ZERO; }

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
                // Loss or tie -> zero uplift/fee
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

// ===============================
// Optional cleanup (unchanged)
// ===============================
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
