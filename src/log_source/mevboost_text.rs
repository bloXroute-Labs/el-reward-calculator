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
use crate::log_source::common::{is_relay_proxy, parse_url};
use log::debug;

// ===============================
// Commit-Boost text regex patterns
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
// MEV logs often have the slot inside msg="getHeader request start - 253 milliseconds into slot 12216021"
lazy_static! {
    pub static ref MEV_GETHEADER_REQ_START_A: Regex = Regex::new(
        r#"msg=\\?"getHeader request start\s*-\s*(\d+)\s*milliseconds into slot\s*(\d+)\\?""#
    ).unwrap();
    // Some deployments log msIntoSlot/slot as kv-pairs too
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
    // If a slotUID is present in the line, use it; otherwise fallback to slot.
    if let Some(uid) = get_kv(line, "slotUID") {
        if !uid.is_empty() {
            return uid;
        }
    }
    // If parentHash present, prefer {slot}_{parentHash} as a pseudo-uid (stable grouping)
    if let Some(ph) = get_kv(line, "parentHash") {
        if !ph.is_empty() {
            return format!("{}_{}", slot, ph);
        }
    }
    slot.to_string()
}

// ===============================
// Commit-Boost: pass 1 (text)
// ===============================
pub fn process_lines_first_pass(line: String, slot_infos: &mut SlotInfos) {
    if let Some(captures) = GETHEADER_REQ_START.captures(&line) {
        let ms_into_slot = captures[1].parse::<i64>().unwrap_or(0);
        let slot = &captures[2];
        let slot_uid = &captures[3];
        debug!("[GETHEADER] slot: {}, slot_uid: {}, ms_into_slot: {}. Line: {}", slot, slot_uid, ms_into_slot, line);

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

        // Parse key=value tokens
        for part in line.split_whitespace() {
            if let Some((key, value)) = part.split_once('=') {
                let key = key.trim();
                let value = value.trim_matches('"');
                match key {
                    "time" => {
                        let date = DateTime::parse_from_rfc3339(value)
                            .expect("failed to parse timestamp")
                            .with_timezone(&Utc);
                        bid.timestamp = date.timestamp();
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

        // Push the bid
        let new_bid_hash = bid.block_hash.clone();
        let new_bid_block_number = bid.block_number.clone();
        slot_info.info.bids.push(bid);

        // Opportunistic immediate resolution if this bid matches a pending payload hash
        if !new_bid_hash.is_empty()
            && slot_info.info.block_hash.is_empty()
            && slot_info.pending_blinded_block_hashes.contains(&new_bid_hash)
        {
            debug!(
                "[RESOLVE] Found pending payload hash {} via bid; locking block_hash (slot_uid={})",
                new_bid_hash, slot_uid
            );
            slot_info.info.block_hash = new_bid_hash.clone();
            if !new_bid_block_number.is_empty() && slot_info.block_number.is_empty() {
                slot_info.block_number = new_bid_block_number;
            }
            // Remove the resolved hash from pending list
            slot_info.pending_blinded_block_hashes.retain(|h| h != &new_bid_hash);
        }

    } else if let Some(captures) = GETPAYLOAD_REQ_START.captures(&line) {
        let ms_into_slot = captures[1].parse::<i64>().unwrap_or(0);
        let slot = &captures[2];
        let slot_uid = &captures[3];

        debug!("[GETPAYLOAD] Processing for slot: {}, slot_uid: {}, ms_into_slot: {}. Line: {}", slot, slot_uid, ms_into_slot, line);
        let slot_info_with_uid = slot_infos.entry(slot.to_string()).or_insert_with(HashMap::new);
        let slot_info = slot_info_with_uid.entry(slot_uid.to_string()).or_insert_with(|| SlotInfo::new(slot_uid.to_string()));

        slot_info.info.payload_start_ms_into_slot = ms_into_slot;

        // Look for blockHash=... on the same line
        if let Some(ph) = get_kv(&line, "blockHash") {
            if !ph.is_empty() {
                let has_matching_bid = slot_info.info.bids.iter().any(|b| b.block_hash == ph);
                if has_matching_bid {
                    if slot_info.info.block_hash.is_empty() || slot_info.info.block_hash != ph {
                        debug!("[SUBMIT] Matching bid present for payload; setting block_hash={} (slot_uid={})", ph, slot_uid);
                        slot_info.info.block_hash = ph;
                    }
                } else {
                    debug!("[DEFER] No matching bid yet for payload {}; storing pending (slot_uid={})", ph, slot_uid);
                    if !slot_info.pending_blinded_block_hashes.contains(&ph) {
                        slot_info.pending_blinded_block_hashes.push(ph);
                    }
                }
            }
        }

    } else if let Some(captures) = PAYLOAD_RECEIVED.captures(&line) {
        let slot = &captures[1];
        let slot_uid = &captures[2];
        debug!("[PAYLOAD_RECEIVED] Processing for slot: {}, slot_uid: {}", slot, slot_uid);
        let slot_info_with_uid = slot_infos.entry(slot.to_string()).or_insert_with(HashMap::new);
        let slot_info = slot_info_with_uid.entry(slot_uid.to_string()).or_insert_with(|| SlotInfo::new(slot_uid.to_string()));
        slot_info.is_payload_received = true;
        // Some deployments log blockHash only in request-start line; nothing to add here.
    }
}

// ===============================
// MEV-Boost: pass 1 (text)
// - Handles cases where slotUID is absent by deriving a stable UID
// - Mirrors commit-boost logic: payload-only lock, pending hash, etc.
// ===============================
pub fn process_lines_first_pass_mev(line: String, slot_infos: &mut SlotInfos) {
    // getHeader request start (two forms)
    if let Some(caps) = MEV_GETHEADER_REQ_START_A.captures(&line) {
        let ms = caps[1].parse::<i64>().unwrap_or(0);
        let slot = &caps[2];
        let slot_uid = get_slot_uid_or(slot, &line);

        debug!("[MEV GETHEADER] slot: {}, slot_uid: {}, ms_into_slot: {}. Line: {}", slot, slot_uid, ms, line);
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

        debug!("[MEV GETHEADER_B] slot: {}, slot_uid: {}, ms_into_slot: {}. Line: {}", slot, slot_uid, ms, line);
        let slot_map = slot_infos.entry(slot.to_string()).or_insert_with(HashMap::new);
        let slot_info = slot_map.entry(slot_uid.clone()).or_insert_with(|| SlotInfo::new(slot_uid));
        slot_info.info.header_start_ms_into_slot = ms;
        slot_info.slot = slot.to_string();
        return;
    }

    // Bid lines – MEV sometimes logs "bid received" or just "best bid"
    if MEV_BID_RECEIVED.is_match(&line) || MEV_BEST_BID.is_match(&line) {
        // slot might be present as kv; if absent we can’t reliably place the bid -> skip
        let Some(slot) = get_kv(&line, "slot") else {
            // try to lift from a parentHash-derived uid if we already created it via header line
            // (Without a slot, we have nowhere safe to store this bid)
            return;
        };
        let slot_uid = get_slot_uid_or(&slot, &line);

        let slot_map = slot_infos.entry(slot.clone()).or_insert_with(HashMap::new);
        let slot_info = slot_map.entry(slot_uid.clone()).or_insert_with(|| SlotInfo::new(slot_uid.clone()));

        let mut bid: Bid = Default::default();
        bid.slot = slot.clone();

        // Parse kv tokens
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
                        // some MEV logs use blockValue instead of value
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

        // Resolve pending payload hash if it matches this bid
        if !new_bid_hash.is_empty()
            && slot_info.info.block_hash.is_empty()
            && slot_info.pending_blinded_block_hashes.contains(&new_bid_hash)
        {
            debug!(
                "[MEV RESOLVE] Found pending payload hash {} via bid; locking block_hash (slot_uid={})",
                new_bid_hash, slot_uid
            );
            slot_info.info.block_hash = new_bid_hash.clone();
            if !new_bid_block_number.is_empty() && slot_info.block_number.is_empty() {
                slot_info.block_number = new_bid_block_number;
            }
            slot_info.pending_blinded_block_hashes.retain(|h| h != &new_bid_hash);
        }
        return;
    }

    // getPayload request start (MEV)
    if let Some(caps) = MEV_GETPAYLOAD_REQ_START.captures(&line) {
        let ms = caps[1].parse::<i64>().unwrap_or(0);
        let slot = &caps[2];
        let slot_uid = get_slot_uid_or(slot, &line);

        debug!("[MEV GETPAYLOAD] slot: {}, slot_uid: {}, ms_into_slot: {}. Line: {}", slot, slot_uid, ms, line);
        let slot_map = slot_infos.entry(slot.to_string()).or_insert_with(HashMap::new);
        let slot_info = slot_map.entry(slot_uid.clone()).or_insert_with(|| SlotInfo::new(slot_uid));

        slot_info.info.payload_start_ms_into_slot = ms;

        if let Some(ph) = get_kv(&line, "blockHash") {
            if !ph.is_empty() {
                let has_matching_bid = slot_info.info.bids.iter().any(|b| b.block_hash == ph);
                if has_matching_bid {
                    if slot_info.info.block_hash.is_empty() || slot_info.info.block_hash != ph {
                        debug!("[MEV SUBMIT] Matching bid present for payload; setting block_hash={}", ph);
                        slot_info.info.block_hash = ph;
                    }
                } else {
                    debug!("[MEV DEFER] No matching bid yet for payload {}; storing pending", ph);
                    if !slot_info.pending_blinded_block_hashes.contains(&ph) {
                        slot_info.pending_blinded_block_hashes.push(ph);
                    }
                }
            }
        }
        return;
    }

    // Payload received (MEV)
    if MEV_PAYLOAD_RECEIVED.is_match(&line) {
        // prefer explicit slot if present
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
// - Payload-preferred, late hash resolution
// - Proxy-preferred top-bid override
// - Non-negative uplift + consistent fee
// ===============================
pub fn finalize_slot_infos(slot_infos: &mut SlotInfos) {
    for (slot, slot_map) in slot_infos.iter_mut() {
        for (slot_uid, slot_info) in slot_map.iter_mut() {
            // Sort bids descending by value
            slot_info.info.bids.sort_by(|a, b| b.bid_value.cmp(&a.bid_value));
            if slot_info.info.bids.is_empty() {
                continue;
            }

            // Late match using pending_blinded_block_hashes: pick the pending hash
            // that has the highest associated bid value
            if slot_info.info.block_hash.is_empty() && !slot_info.pending_blinded_block_hashes.is_empty() {
                let mut best: Option<(String, Decimal)> = None;
                for ph in &slot_info.pending_blinded_block_hashes {
                    let max_for_ph = slot_info.info.bids
                        .iter()
                        .filter(|b| &b.block_hash == ph)
                        .map(|b| b.bid_value)
                        .max();
                    if let Some(maxv) = max_for_ph {
                        match best {
                            Some((_, ref cur)) if maxv <= *cur => {}
                            _ => best = Some((ph.clone(), maxv)),
                        }
                    }
                }
                if let Some((best_hash, _)) = best {
                    debug!("[FINALIZE] Late-match resolved from pending hash -> {}", best_hash);
                    slot_info.info.block_hash = best_hash;
                }
            }

            // If still no winner, fallback to best bid with a non-empty block_hash
            if slot_info.info.block_hash.is_empty() {
                if let Some(best_bid) = slot_info.info.bids.iter().find(|b| !b.block_hash.is_empty()) {
                    debug!("[AUTO-MATCH] No payload-set hash; falling back to best bid {}", best_bid.block_hash);
                    slot_info.info.block_hash = best_bid.block_hash.clone();
                } else {
                    continue;
                }
            }

            // Proxy-preferred top hash override when payload-matched value is lower than the top value
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

            // Find the winning bid by chosen hash (fallback again if not found)
            let winning_block_hash = slot_info.info.block_hash.clone();
            let mut winner_index = slot_info.info.bids.iter().position(|b| b.block_hash == winning_block_hash);
            if winner_index.is_none() {
                if let Some(i) = slot_info.info.bids.iter().position(|b| !b.block_hash.is_empty()) {
                    slot_info.info.block_hash = slot_info.info.bids[i].block_hash.clone();
                    winner_index = Some(i);
                } else {
                    continue;
                }
            }
            let winner_idx = winner_index.unwrap();
            let bid = &slot_info.info.bids[winner_idx];

            // Capture block_number if present
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
                    .map_or(String::new(), |b| parse_url(b));

                // Clamp negative uplift
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
                    let el_reward_increase_wei_decimal = (el_reward_increase * wei_multiplier).round();
                    let el_reward_increase_wei =
                        U256::from_dec_str(&el_reward_increase_wei_decimal.to_string())
                            .unwrap_or(U256::zero());

                    let el_reward_percent_precise =
                        (el_reward_increase / slot_info.onchain_bid_value) * Decimal::from(100);

                    slot_info.el_reward_increase_wei = el_reward_increase_wei;
                    slot_info.el_reward_increase_eth = el_reward_increase;
                    slot_info.el_reward_increase_percent_precise = el_reward_percent_precise;
                    slot_info.el_reward_increase_percentage = el_reward_percent_precise.round().to_u64().unwrap_or(0);

                    // Fee tiers from positive uplift only
                    slot_info.fee_per_block = if el_reward_percent_precise <= dec!(1) {
                        dec!(0.0)
                    } else if el_reward_percent_precise <= dec!(5) {
                        if el_reward_increase >= dec!(0.0015) { dec!(0.0015) } else { dec!(0.0) }
                    } else if el_reward_percent_precise <= dec!(9) {
                        if el_reward_increase > dec!(0.003) { dec!(0.003) }
                        else if el_reward_increase > dec!(0.0015) { dec!(0.0015) }
                        else { dec!(0.0) }
                    } else {
                        if el_reward_increase > dec!(0.005) { dec!(0.005) }
                        else if el_reward_increase > dec!(0.003) { dec!(0.003) }
                        else if el_reward_increase > dec!(0.0015) { dec!(0.0015) }
                        else { dec!(0.0) }
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

// ===============================
// Optional: if you still want to drop slot_uids without any relay-proxy bids
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
