use lazy_static::lazy_static;
use regex::Regex;
use crate::{SlotInfo, SlotInfos};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use crate::Bid;
use url::Url;
use ethers::types::U256;
use ethers::utils::parse_ether;
use ethers::utils::format_units;
use crate::log_source::common::is_relay_proxy;
use crate::parse_url;
use log::debug;

lazy_static! {
    pub static ref GETHEADER_REQ_START: Regex =
        Regex::new(r"getHeader request start.*?msIntoSlot=(\d+).*?slot=(\d+).*?slotUID=([\w\-]+)")
            .unwrap();
    pub static ref BID_RECEIVED: Regex =
        Regex::new(r#"msg=\\?"bid received\\?"(?:\s+\S+)*\s+slot=(\d+)\s+slotUID=([\w\-]+)"#).unwrap();
    pub static ref GETPAYLOAD_REQ_START: Regex = Regex::new(
        r"submitBlindedBlock request start.*?msIntoSlot=(\d+).*?slot=(\d+).*?slotUID=([\w\-]+)"
    )
    .unwrap();
    pub static ref PAYLOAD_RECEIVED: Regex =
        Regex::new(r"received payload from relay.*?slot=(\d+).*?slotUID=([\w\-]+)").unwrap();
}

pub fn process_lines(line: String, slot_infos: &mut SlotInfos) {
    // Process GETHEADER request start lines:
    if let Some(captures) = GETHEADER_REQ_START.captures(&line) {
        let ms_into_slot = captures.get(1).unwrap().as_str().parse::<i64>().unwrap_or(0);
        let slot = captures.get(2).unwrap().as_str();
        let slot_uid = captures.get(3).unwrap().as_str();
        debug!("[GETHEADER] slot: {}, slot_uid: {}, ms_into_slot: {}. Line: {}", slot, slot_uid, ms_into_slot, line);

        // Retrieve or create the inner map for this slot.
        let slot_info_with_uid = slot_infos.entry(slot.to_string())
            .or_insert_with(HashMap::new);

        // If an entry for this slot_uid already exists, update its header info.
        if let Some(existing) = slot_info_with_uid.get_mut(slot_uid) {
            debug!("[GETHEADER] Updating existing SlotInfo for slot_uid: {}", slot_uid);
            existing.info.header_start_ms_into_slot = ms_into_slot;
            existing.slot = slot.to_string();
        } else {
            debug!("[GETHEADER] Creating new SlotInfo for slot_uid: {}", slot_uid);
            let mut slot_info = SlotInfo::new(slot_uid.to_string());
            slot_info.info.header_start_ms_into_slot = ms_into_slot;
            slot_info.slot = slot.to_string();
            slot_info_with_uid.insert(slot_uid.to_string(), slot_info);
        }
    }
    // Process BID_RECEIVED lines.
    else if let Some(captures) = BID_RECEIVED.captures(&line) {
        let slot = captures.get(1).unwrap().as_str();
        let slot_uid = captures.get(2).unwrap().as_str();
        debug!("[BID_RECEIVED] Processing bid for slot: {}, slot_uid: {}. Line: {}", slot, slot_uid, line);

        // Retrieve or create the inner map for this slot.
        let slot_info_with_uid = slot_infos.entry(slot.to_string())
            .or_insert_with(|| {
                debug!("[BID_RECEIVED] No inner map for slot {} found. Creating one.", slot);
                HashMap::new()
            });
        // Retrieve or create the SlotInfo for this slot_uid.
        let slot_info = slot_info_with_uid.entry(slot_uid.to_string())
            .or_insert_with(|| {
                debug!("[BID_RECEIVED] slot_uid {} not found for slot {}. Creating default SlotInfo.", slot_uid, slot);
                SlotInfo::new(slot_uid.to_string())
            });

        let mut bid: Bid = Default::default();
        bid.slot = slot.to_string();

        // Process each key-value pair in the line.
        for part in line.split_whitespace() {
            debug!("[BID_RECEIVED] Processing part: {}", part);
            if let Some((key, value)) = part.split_once("=") {
                let key = key.trim();
                let value = value.trim_matches('"');
                debug!("[BID_RECEIVED] Parsed key: '{}', value: '{}'", key, value);
                match key {
                    "time" => {
                        let date = DateTime::parse_from_rfc3339(value)
                            .expect(&format!("failed to parse timestamp for slot-{}, timestamp-{}", slot, value))
                            .with_timezone(&Utc);
                        bid.timestamp = date.to_utc().timestamp();
                        debug!("[BID_RECEIVED] Parsed time: {}", bid.timestamp);
                    },
                    "blockHash" => {
                        bid.block_hash = value.to_string();
                        debug!("[BID_RECEIVED] Parsed blockHash: {}", bid.block_hash);
                    },
                    "parentHash" => {
                        bid.parent_hash = value.to_string();
                        debug!("[BID_RECEIVED] Parsed parentHash: {}", bid.parent_hash);
                    },
                    "pubkey" => {
                        bid.pubkey = value.to_string();
                        debug!("[BID_RECEIVED] Parsed pubkey: {}", bid.pubkey);
                    },
                    "blockNumber" => {
                        bid.block_number = value.to_string();
                        debug!("[BID_RECEIVED] Parsed blockNumber: {}", bid.block_number);
                    },
                    "ua" => {
                        bid.ua = value.to_string();
                        debug!("[BID_RECEIVED] Parsed ua: {}", bid.ua);
                    },
                    "value" => {
                        bid.bid_value = value.parse::<f64>().unwrap_or(0.0);
                        debug!("[BID_RECEIVED] Parsed value: {}", bid.bid_value);
                    },
                    "url" => {
                        if let Ok(url) = Url::parse(value) {
                            bid.relay = url.domain().unwrap_or(value).to_string();
                        } else {
                            bid.relay = value.to_string();
                        }
                        debug!("[BID_RECEIVED] Parsed relay: {}", bid.relay);
                    },
                    _ => {
                        debug!("[BID_RECEIVED] Unrecognized key: {}", key);
                    }
                }
            }
        }
        debug!("[BID_RECEIVED] Final bid: {:?}", bid);
        slot_info.info.bids.push(bid);
        debug!("[BID_RECEIVED] Bid added. Total bids now: {}", slot_info.info.bids.len());
    }
    // Process GETPAYLOAD request start lines.
    else if let Some(captures) = GETPAYLOAD_REQ_START.captures(&line) {
        let ms_into_slot = captures.get(1).unwrap().as_str().parse::<i64>().unwrap_or(0);
        let slot = captures.get(2).unwrap().as_str();
        let slot_uid = captures.get(3).unwrap().as_str();

        debug!("[GETPAYLOAD] Processing for slot: {}, slot_uid: {}, ms_into_slot: {}. Line: {}", slot, slot_uid, ms_into_slot, line);
        // Retrieve or create the inner map for this slot.
        let slot_info_with_uid = slot_infos.entry(slot.to_string())
            .or_insert_with(HashMap::new);
        // Retrieve or create the SlotInfo for this slot_uid.
        let slot_info = slot_info_with_uid.entry(slot_uid.to_string())
            .or_insert_with(|| {
                debug!("[GETPAYLOAD] slot_uid {} not found for slot {}. Creating default SlotInfo.", slot_uid, slot);
                SlotInfo::new(slot_uid.to_string())
            });

        slot_info.info.payload_start_ms_into_slot = ms_into_slot;
        let parts: Vec<&str> = line.split_whitespace().collect();
        for part in parts {
            if let Some((key, value)) = part.split_once("=") {
                let key = key.trim();
                let value = value.trim_matches('"');
                if key == "blockHash" {
                    debug!("[GETPAYLOAD] Found blockHash: {}", value);
                    slot_info.info.block_hash = value.to_string();
                    // Sort bids by bid value (descending).
                    slot_info.info.bids.sort_by(|a, b| b.bid_value.total_cmp(&a.bid_value));

                    if let Some(bid) = slot_info.info.bids.iter().find(|bid| bid.block_hash == value) {
                        slot_info.block_number = bid.block_number.clone();
                        slot_info.onchain_bid_delivered_relay = parse_url(bid);
                        slot_info.onchain_bid_value = bid.bid_value;
                        let highest_bid = slot_info.info.bids.get(0).unwrap();
                        slot_info.is_winning_bid_highest = if value == highest_bid.block_hash {
                            true
                        } else {
                            bid.bid_value == highest_bid.bid_value
                        };

                        let highest_bidders: Vec<_> = slot_info.info.bids.iter()
                            .filter(|bid| bid.bid_value == highest_bid.bid_value)
                            .map(|bid| bid.relay.clone())
                            .collect();

                        let relay_proxy_bidders: Vec<_> = highest_bidders.iter()
                            .filter(|relay| !is_relay_proxy(relay))
                            .cloned()
                            .collect();
                        slot_info.equal_to_proxy_bidders = relay_proxy_bidders.join(", ");
                        slot_info.is_equal_to_proxy_bid = !relay_proxy_bidders.is_empty();
                        slot_info.is_proxy_win = is_relay_proxy(&bid.relay);

                        debug!("[GETPAYLOAD] Processed bids. Highest bid: {:?}, relay_proxy_bidders: {:?}", highest_bid, relay_proxy_bidders);

                        if highest_bidders.len() > 1 && !slot_info.is_proxy_win {
                            debug!("[GETPAYLOAD] Skipping processing as relay-proxy didn't win and multiple highest bids exist.");
                            return;
                        }

                        if slot_info.is_proxy_win {
                            let second_best_bid = slot_info.info.bids.iter()
                                .skip(1)
                                .find(|bid| !is_relay_proxy(&bid.relay));

                            let second_best_val = second_best_bid.map_or(0.0, |bid| bid.bid_value);
                            slot_info.second_highest_bid_value = second_best_val;
                            slot_info.second_higher_bid_delivered_relay = second_best_bid
                                .map_or(String::new(), |bid| parse_url(bid));

                            if !slot_info.is_equal_to_proxy_bid && second_best_val > 0.0 {
                                let el_reward_increase = slot_info.onchain_bid_value - second_best_val;
                                let el_reward_increase_wei: U256 = parse_ether(&el_reward_increase.to_string()).expect("Invalid Ether value");
                                let el_reward_increase_eth = format_units(el_reward_increase_wei, "ether").expect("Formatting failed")
                                    .parse::<f64>().unwrap();
                                let el_reward_increase_percent_precise = (el_reward_increase / slot_info.onchain_bid_value) * 100.0;
                                slot_info.el_reward_increase_wei = el_reward_increase_wei;
                                slot_info.el_reward_increase_eth = el_reward_increase_eth;
                                slot_info.el_reward_increase_percent_precise = el_reward_increase_percent_precise;
                                slot_info.el_reward_increase_percentage = el_reward_increase_percent_precise.round() as u64;
                            }
                        }
                    }
                }
            }
        }
    }
    // Process PAYLOAD_RECEIVED lines.
    else if let Some(captures) = PAYLOAD_RECEIVED.captures(&line) {
        let slot = captures.get(1).unwrap().as_str();
        let slot_uid = captures.get(2).unwrap().as_str();
        debug!("[PAYLOAD_RECEIVED] Processing for slot: {}, slot_uid: {}", slot, slot_uid);
        // Retrieve or create the inner map if missing.
        let slot_info_with_uid = slot_infos.entry(slot.to_string())
            .or_insert_with(HashMap::new);
        let slot_info = slot_info_with_uid.entry(slot_uid.to_string())
            .or_insert_with(|| {
                debug!("[PAYLOAD_RECEIVED] slot_uid {} not found for slot {}. Creating default SlotInfo.", slot_uid, slot);
                SlotInfo::new(slot_uid.to_string())
            });
        slot_info.is_payload_received = true;
    }
}
