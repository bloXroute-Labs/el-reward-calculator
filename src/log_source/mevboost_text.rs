use lazy_static::lazy_static;
use regex::Regex;
use rust_decimal::Decimal;
use rust_decimal::prelude::ToPrimitive;
use crate::{SlotInfo, SlotInfos};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use crate::Bid;
use url::Url;
use ethers::types::U256;
use crate::log_source::common::{is_relay_proxy, parse_url};
use log::debug;

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
        let mut payload_block_hash: Option<String> = None;
        for part in line.split_whitespace() {
            if let Some((key, value)) = part.split_once('=') {
                if key.trim() == "blockHash" {
                    payload_block_hash = Some(value.trim_matches('"').to_string());
                    break;
                }
            }
        }

        if let Some(ph) = payload_block_hash {
            if !ph.is_empty() {
                // If there is already a matching bid, lock immediately; otherwise store as pending
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

pub fn finalize_slot_infos(slot_infos: &mut SlotInfos) {
    for slot_info_with_uid in slot_infos.values_mut() {
        for slot_info in slot_info_with_uid.values_mut() {
            // Sort bids descending by value for consistent highest-bid checks
            slot_info.info.bids.sort_by(|a, b| b.bid_value.cmp(&a.bid_value));

            // 1) Late match using pending_blinded_block_hashes: pick the hash with the highest associated bid value
            if slot_info.info.block_hash.is_empty() && !slot_info.pending_blinded_block_hashes.is_empty() {
                let mut best: Option<(String /*hash*/, Decimal /*max value*/)> = None;

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

            // 2) If still no winner, fallback to best bid with a non-empty block_hash
            if slot_info.info.block_hash.is_empty() {
                if let Some(best_bid) = slot_info.info.bids.iter().find(|b| !b.block_hash.is_empty()) {
                    debug!("[AUTO-MATCH] No payload-set hash; falling back to best bid {}", best_bid.block_hash);
                    slot_info.info.block_hash = best_bid.block_hash.clone();
                } else {
                    // Nothing to do for this slot_uid
                    continue;
                }
            }

            // Clone to avoid borrowing across potential mutations/closures (prevents E0506)
            let mut winning_block_hash = slot_info.info.block_hash.clone();

            // 3) Try to find the winning bid by block_hash
            let mut winner_index = slot_info.info.bids.iter().position(|b| b.block_hash == winning_block_hash);

            // If the payload-selected hash never appears in bids, fallback again to highest non-empty hash
            if winner_index.is_none() {
                if let Some(best_bid_idx) = slot_info.info.bids.iter().position(|b| !b.block_hash.is_empty()) {
                    let best_bid = &slot_info.info.bids[best_bid_idx];
                    debug!(
                        "[AUTO-MATCH] No bid matched payload hash {}; using best bid {}",
                        winning_block_hash, best_bid.block_hash
                    );
                    slot_info.info.block_hash = best_bid.block_hash.clone();
                    winning_block_hash = best_bid.block_hash.clone();
                    winner_index = Some(best_bid_idx);
                } else {
                    continue;
                }
            }

            // Safe to unwrap now
            let winner_idx = winner_index.unwrap();
            let bid = &slot_info.info.bids[winner_idx];

            // Block number from winning bid if available
            if slot_info.block_number.is_empty() && !bid.block_number.is_empty() {
                slot_info.block_number = bid.block_number.clone();
            }

            // Winner + highest-bid/equality computations
            slot_info.onchain_bid_delivered_relay = parse_url(bid);
            slot_info.onchain_bid_value = bid.bid_value;

            if let Some(highest_bid) = slot_info.info.bids.first() {
                slot_info.is_winning_bid_highest =
                    winning_block_hash == highest_bid.block_hash || bid.bid_value == highest_bid.bid_value;

                // Determine ties at the highest bid value
                let highest_bidders: Vec<_> = slot_info.info.bids
                    .iter()
                    .filter(|b| b.bid_value == highest_bid.bid_value)
                    .map(|b| b.relay.clone())
                    .collect();

                let relay_proxy_bidders: Vec<_> = highest_bidders.iter()
                    .filter(|r| !is_relay_proxy(r))
                    .cloned()
                    .collect();

                slot_info.equal_to_proxy_bidders = relay_proxy_bidders.join(", ");
                slot_info.is_equal_to_proxy_bid = !relay_proxy_bidders.is_empty();
                slot_info.is_proxy_win = is_relay_proxy(&bid.relay);

                // If multiple highest bids exist and proxy did not win, skip EL reward calc
                if highest_bidders.len() > 1 && !slot_info.is_proxy_win {
                    continue;
                }

                // Proxy win → compute uplift vs best non-proxy competitor
                if slot_info.is_proxy_win {
                    let second_best_bid = slot_info.info.bids.iter()
                        .skip(1)
                        .find(|b| !is_relay_proxy(&b.relay));

                    let second_best_val = second_best_bid.map_or(Decimal::ZERO, |b| b.bid_value);
                    slot_info.second_highest_bid_value = second_best_val;
                    slot_info.second_higher_bid_delivered_relay = second_best_bid
                        .map_or(String::new(), |b| parse_url(b));

                    if !slot_info.is_equal_to_proxy_bid && second_best_val > Decimal::ZERO {
                        let el_reward_increase = slot_info.onchain_bid_value - second_best_val;
                        let wei_multiplier = Decimal::from(1_000_000_000_000_000_000u128);
                        let el_reward_increase_wei_decimal = (el_reward_increase * wei_multiplier).round();

                        let el_reward_increase_wei: U256 =
                            U256::from_dec_str(&el_reward_increase_wei_decimal.to_string())
                                .unwrap_or_else(|_| U256::zero());

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
            }
        }
    }
}
