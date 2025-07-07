use crate::{SlotInfo, SlotInfos };
use crate::log_source::types::{LogEntryMEVBoost, Bid, SlotTrait};
use serde_json::{self, Deserializer, Value};
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use crate::log_source::common::parse_url;
use ethers::types::U256;
use ethers::utils::parse_ether;
use ethers::utils::format_units;

pub fn parse_file_content<R: std::io::Read>(reader : R , slot_infos :&mut SlotInfos){
    let stream = Deserializer::from_reader(reader).into_iter::<Value>();
    for entry in stream {
        match entry {
            Ok(Value::Object(map)) => {
                match serde_json::from_value::<LogEntryMEVBosst>(Value::Object(map)) {
                    Ok(log_entry) => {
                        // Process the valid log entry
                        process_json(&log_entry, slot_infos);
                    }
                    Err(e) => {
                        eprintln!("Failed to parse log entry: {}. Skipping.", e);
                    }
                }
            }
            Ok(Value::Null) => {
                eprintln!("Encountered Null value. Skipping.");
            }
            Ok(Value::Bool(_)) => {
                eprintln!("Encountered Boolean value. Skipping.");
            }
            Ok(Value::Number(_)) => {
                eprintln!("Encountered Number value. Skipping.");
            }
            Ok(Value::String(_)) => {
                eprintln!("Encountered String value. Skipping.");
            }
            Ok(Value::Array(vec)) => {
                for item in vec {
                    match serde_json::from_value::<LogEntryMEVBoost>(item) {
                        Ok(log_entry) => {
                            // Process the valid log entry
                            process_json(&log_entry, slot_infos);
                        }
                        Err(e) => {
                            eprintln!("Failed to parse log entry: {}. Skipping.", e);
                        }
                    }
                }
            }
            Err(e) => {
                eprintln!("Failed to parse JSON entry: {}. Skipping.", e);
                // Optionally, log the error or take other actions
            }
        }
    }
    finalize_payloads(slot_infos);
}

fn process_json(log_entry: &LogEntryMEVBoost, slot_infos: &mut SlotInfos){
    let slot = log_entry.slot.clone();
    let slot_uid = log_entry.slot_uid.clone();

    let slot_info_map = slot_infos.entry(slot.clone()).or_insert_with(HashMap::new);

    let slot_info = slot_info_map.entry(slot_uid.clone()).or_insert_with(|| {
        SlotInfo::new_with_slot_uid_and_slot(slot_uid.to_string(), slot.to_string())
    });
    println!("Method: {}", log_entry.method);
    match log_entry.method.as_str() {
        "getHeader" => {
            match log_entry.msg.as_str() {
                "bid received" => {
                    let mut bid: Bid = Default::default();
                    // Extract time
                    let date = DateTime::parse_from_rfc3339(&log_entry.time)
                        .expect(&format!(
                            "failed to parse timestamp for slot-{}, timestamp-{}",
                            slot.clone(),
                            log_entry.time.clone()
                        ))
                        .with_timezone(&Utc);
                    bid.timestamp = date.to_utc().timestamp();
                    bid.slot = log_entry.slot.clone();
                    bid.block_hash = log_entry.block_hash.clone();
                    bid.parent_hash = log_entry.parent_hash.clone();
                    bid.ua = log_entry.ua.clone();
                    bid.relay = log_entry.url.to_string();
                    bid.pubkey = log_entry.pubkey.clone().unwrap_or_else(|| "N/A".to_string());
                    bid.block_number = log_entry.block_number
                        .map(|num| num.to_string())
                        .unwrap_or_else(|| "N/A".to_string());
                    bid.bid_value = log_entry.value
                        .as_deref()
                        .unwrap_or("0.0")
                        .parse::<f64>()
                        .unwrap_or(0.0);

                    slot_info.info.bids.push(bid);
                }
                _ => {}
            }
        }

        "getPayload" => {
            match log_entry.msg.as_str() {
                "received payload from relay" => {
                    slot_info.is_payload_received = true;
                    slot_info.info.block_hash = log_entry.block_hash.clone();
                    println!("inside getpayload: {}",log_entry.block_hash.clone());
                }
                _ => {}
            }
        }
        _ => {}
    }
}

// If any slot_uid in a slot has relay-proxy win, keep only those entries
//If no proxy win exists for that slot, retain only the first slot_uid entry
pub fn finalize_payloads(slot_infos: &mut SlotInfos) {
    for (_slot, slot_info_map) in slot_infos.iter_mut() {
        // Track whether any proxy win is found
        let mut found_proxy_win = false;
        let mut proxy_win_keys = vec![];

        // First pass: compute all values and find if proxy win exists
        for (slot_uid, slot_info) in slot_info_map.iter_mut() {
            if !slot_info.is_payload_received {
                continue;
            }

            // Sort bids by value
            slot_info.info.bids.sort_by(|a, b| b.bid_value.total_cmp(&a.bid_value));
            let block_hash = &slot_info.info.block_hash;

            for bid in &slot_info.info.bids {
                if &bid.block_hash == block_hash {
                    slot_info.block_number = bid.block_number.clone();
                    slot_info.is_proxy_win = bid.relay.contains("relay-proxy");
                    if slot_info.is_proxy_win {
                        found_proxy_win = true;
                        proxy_win_keys.push(slot_uid.clone());
                    }

                    let highest_bid = slot_info.info.bids.first().unwrap();
                    slot_info.onchain_bid_delivered_relay = parse_url(bid);
                    slot_info.onchain_bid_value = bid.bid_value;

                    slot_info.is_winning_bid_highest = if &highest_bid.block_hash == block_hash {
                        true
                    } else {
                        match slot_info.info.bids.iter().find(|b| b.block_hash == *block_hash) {
                            Some(winning_bid) => winning_bid.bid_value == highest_bid.bid_value,
                            None => false,
                        }
                    };

                    let highest_bidders: Vec<_> = slot_info.info.bids
                        .iter()
                        .filter(|b| b.bid_value == highest_bid.bid_value)
                        .map(|b| b.relay.clone())
                        .collect();

                    let relay_proxy_bidders: Vec<_> = highest_bidders.iter()
                        .filter(|relay| !relay.contains("relay-proxy"))
                        .cloned()
                        .collect();

                    slot_info.equal_to_proxy_bidders = relay_proxy_bidders.join(", ");

                    if slot_info.is_proxy_win && slot_info.is_winning_bid_highest {
                        let second_best_bid = slot_info.info.bids.iter()
                            .skip(1)
                            .find(|bid| !bid.relay.contains("relay-proxy"));

                        let second_best_bid_value = second_best_bid.map_or(0.0, |b| b.bid_value);
                        let second_best_relay = second_best_bid.map_or(String::new(), |b| parse_url(b));
                        slot_info.second_highest_bid_value = second_best_bid_value;
                        slot_info.second_higher_bid_delivered_relay = second_best_relay;

                        if second_best_bid_value > 0.0 {
                            let el_reward_increase = slot_info.onchain_bid_value - second_best_bid_value;
                            let el_reward_increase_wei: U256 = parse_ether(&el_reward_increase.to_string()).unwrap();
                            let el_reward_increase_eth = format_units(el_reward_increase_wei, "ether").unwrap();
                            let percent_precise = (el_reward_increase / slot_info.onchain_bid_value) * 100.0;

                            slot_info.el_reward_increase_eth = el_reward_increase_eth.parse::<f64>().unwrap();
                            slot_info.el_reward_increase_percent_precise = percent_precise;
                            slot_info.el_reward_increase_percentage = percent_precise.round() as u64;
                        }
                    }

                    break;
                }
            }
        }

        // Second pass: Retain only proxy win entries or one fallback if none found
        let keep_keys: Vec<String> = if found_proxy_win {
            proxy_win_keys
        } else {
            slot_info_map.keys().next().map(|k| vec![k.clone()]).unwrap_or_default()
        };

        slot_info_map.retain(|uid, _| keep_keys.contains(uid));
    }
}



//
// The below code will be able to get all the bid value from slot uuid
//

//pub fn finalize_payloads(slot_infos: &mut SlotInfos) {
//    for (_slot, slot_info_map) in slot_infos.iter_mut() {
//        for (_slot_uid, slot_info) in slot_info_map.iter_mut() {
//            if !slot_info.is_payload_received {
//                continue;
//            }
//
//            // Sort bids by value
//            slot_info.info.bids.sort_by(|a, b| b.bid_value.total_cmp(&a.bid_value));
//            let block_hash = &slot_info.info.block_hash;
//
//            for bid in &slot_info.info.bids {
//                if &bid.block_hash == block_hash {
//                    slot_info.block_number = bid.block_number.clone();
//                    slot_info.is_proxy_win = bid.relay.contains("relay-proxy") || bid.relay.contains("44.213.66.139") || bid.relay.contains("35.175.121.222");
//
//                    let highest_bid = slot_info.info.bids.first().unwrap();
//                    slot_info.onchain_bid_delivered_relay = parse_url(bid);
//                    slot_info.onchain_bid_value = bid.bid_value;
//
//                    slot_info.is_winning_bid_highest = if &highest_bid.block_hash == block_hash {
//                        true
//                    } else {
//                        match slot_info.info.bids.iter().find(|b| b.block_hash == *block_hash) {
//                            Some(winning_bid) => winning_bid.bid_value == highest_bid.bid_value,
//                            None => false,
//                        }
//                    };
//
//                    let highest_bidders: Vec<_> = slot_info.info.bids
//                        .iter()
//                        .filter(|b| b.bid_value == highest_bid.bid_value)
//                        .map(|b| b.relay.clone())
//                        .collect();
//
//                    let relay_proxy_bidders: Vec<_> = highest_bidders.iter()
//                        .filter(|relay| !relay.contains("relay-proxy") && !relay.contains("44.213.66.139") && !relay.contains("35.175.121.222"))
//                        .cloned()
//                        .collect();
//
//                    slot_info.equal_to_proxy_bidders = relay_proxy_bidders.join(", ");
//
//                    if slot_info.is_proxy_win && slot_info.is_winning_bid_highest {
//                        let second_best_bid = slot_info.info.bids.iter()
//                            .skip(1)
//                            .find(|bid| !bid.relay.contains("relay-proxy")
//                                && !bid.relay.contains("44.213.66.139")
//                                && !bid.relay.contains("35.175.121.222"));
//
//                        let second_best_bid_value = second_best_bid.map_or(0.0, |b| b.bid_value);
//                        let second_best_relay = second_best_bid.map_or(String::new(), |b| parse_url(b));
//                        slot_info.second_highest_bid_value = second_best_bid_value;
//                        slot_info.second_higher_bid_delivered_relay = second_best_relay;
//
//                        if second_best_bid_value > 0.0 {
//                            let el_reward_increase = slot_info.onchain_bid_value - second_best_bid_value;
//                            let el_reward_increase_wei: U256 = parse_ether(&el_reward_increase.to_string()).unwrap();
//                            let el_reward_increase_eth = format_units(el_reward_increase_wei, "ether").unwrap();
//                            let percent_precise = (el_reward_increase / slot_info.onchain_bid_value) * 100.0;
//
//                            slot_info.el_reward_increase_eth = el_reward_increase_eth.parse::<f64>().unwrap();
//                            slot_info.el_reward_increase_percent_precise = percent_precise;
//                            slot_info.el_reward_increase_percentage = percent_precise.round() as u64;
//                        }
//                    }
//
//                    break;
//                }
//            }
//        }
//    }
//}
