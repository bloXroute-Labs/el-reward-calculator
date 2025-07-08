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
use ethers::utils::format_units;
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

        for part in line.split_whitespace() {
            if let Some((key, value)) = part.split_once("=") {
                let key = key.trim();
                let value = value.trim_matches('"');
                match key {
                    "time" => {
                        let date = DateTime::parse_from_rfc3339(value)
                            .expect("failed to parse timestamp")
                            .with_timezone(&Utc);
                        bid.timestamp = date.timestamp();
                    }
                    "blockHash" => bid.block_hash = value.to_string(),
                    "parentHash" => bid.parent_hash = value.to_string(),
                    "pubkey" => bid.pubkey = value.to_string(),
                    "blockNumber" => bid.block_number = value.to_string(),
                    "ua" => bid.ua = value.to_string(),
                    "value" => bid.bid_value = value.parse::<Decimal>().unwrap_or_default(),
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
        slot_info.info.bids.push(bid);
    } else if let Some(captures) = GETPAYLOAD_REQ_START.captures(&line) {
        let ms_into_slot = captures[1].parse::<i64>().unwrap_or(0);
        let slot = &captures[2];
        let slot_uid = &captures[3];

        debug!("[GETPAYLOAD] Processing for slot: {}, slot_uid: {}, ms_into_slot: {}. Line: {}", slot, slot_uid, ms_into_slot, line);
        let slot_info_with_uid = slot_infos.entry(slot.to_string()).or_insert_with(HashMap::new);
        let slot_info = slot_info_with_uid.entry(slot_uid.to_string()).or_insert_with(|| SlotInfo::new(slot_uid.to_string()));

        slot_info.info.payload_start_ms_into_slot = ms_into_slot;

        for part in line.split_whitespace() {
            if let Some((key, value)) = part.split_once("=") {
                if key.trim() == "blockHash" {
                    slot_info.info.block_hash = value.trim_matches('"').to_string();
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
    }
}

pub fn finalize_slot_infos(slot_infos: &mut SlotInfos) {
    for slot_info_with_uid in slot_infos.values_mut() {
        for slot_info in slot_info_with_uid.values_mut() {
            let block_hash = &slot_info.info.block_hash;
            if block_hash.is_empty() {
                continue;
            }

            slot_info.info.bids.sort_by(|a, b| b.bid_value.cmp(&a.bid_value));

            if let Some(bid) = slot_info.info.bids.iter().find(|bid| &bid.block_hash == block_hash) {
                slot_info.block_number = bid.block_number.clone();
                slot_info.onchain_bid_delivered_relay = parse_url(bid);
                slot_info.onchain_bid_value = bid.bid_value;

                let highest_bid = slot_info.info.bids.first().unwrap();
                slot_info.is_winning_bid_highest = block_hash == &highest_bid.block_hash || bid.bid_value == highest_bid.bid_value;

                let highest_bidders: Vec<_> = slot_info.info.bids.iter()
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

                if highest_bidders.len() > 1 && !slot_info.is_proxy_win {
                    continue;
                }

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

                        let el_reward_increase_wei: U256 = U256::from_dec_str(&el_reward_increase_wei_decimal.to_string())
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
