
use csv::WriterBuilder;
use crate::{  SlotInfo};
use crate::log_source::types::{ CommitBoostSlotInfo, SlotInfoWithoutBids};
use serde_json::{self};
use log::debug;
use ethers::types::U256;
use std::fmt::Debug;
use serde::Serialize;
use std::collections::HashMap;
use std::fs::File;
use std::io::{Write, Result as IoResult};

pub trait RewardStats: Clone {
    fn get_uid(&self) -> &str;
    fn get_slot(&self) -> &str;
    fn get_block_number(&self) -> &str;
    fn get_block_hash(&self) -> &str;
    fn get_header_start(&self) -> i64;
    fn get_payload_start(&self) -> i64;
    fn get_onchain_bid_value(&self) -> f64;
    fn get_el_reward_eth(&self) -> f64;
    fn get_el_reward_wei(&self) -> U256;
    fn get_is_proxy_win(&self) -> bool;
    fn get_is_winning_bid_highest(&self) -> bool;
    fn get_second_highest_bid_value(&self) -> f64;
    fn get_second_higher_bid_delivered_relay(&self) -> &str;
    fn get_onchain_bid_delivered_relay(&self) -> &str;
    fn get_is_payload_received(&self) -> bool;
    fn get_el_reward_percentage(&self) -> u64;
    fn get_el_reward_precise(&self) -> f64;
    fn get_equal_to_proxy_bidders(&self) -> &str;
    fn is_equal_to_proxy_bid(&self) -> bool;
    fn get_fee_per_block(&self) -> f64;
}

impl RewardStats for SlotInfo {
    fn get_uid(&self) -> &str { &self.slot_uid }
    fn get_slot(&self) -> &str { &self.slot }
    fn get_block_number(&self) -> &str { &self.block_number }
    fn get_block_hash(&self) -> &str { &self.info.block_hash }
    fn get_header_start(&self) -> i64 { self.info.header_start_ms_into_slot }
    fn get_payload_start(&self) -> i64 { self.info.payload_start_ms_into_slot }
    fn get_onchain_bid_value(&self) -> f64 { self.onchain_bid_value }
    fn get_el_reward_eth(&self) -> f64 { self.el_reward_increase_eth }
    fn get_el_reward_wei(&self) -> U256 { self.el_reward_increase_wei.clone() }
    fn get_is_proxy_win(&self) -> bool { self.is_proxy_win }
    fn get_is_winning_bid_highest(&self) -> bool { self.is_winning_bid_highest }
    fn get_second_highest_bid_value(&self) -> f64 { self.second_highest_bid_value }
    fn get_second_higher_bid_delivered_relay(&self) -> &str { &self.second_higher_bid_delivered_relay }
    fn get_onchain_bid_delivered_relay(&self) -> &str { &self.onchain_bid_delivered_relay }
    fn get_is_payload_received(&self) -> bool { self.is_payload_received }
    fn get_el_reward_percentage(&self) -> u64 { self.el_reward_increase_percentage }
    fn get_el_reward_precise(&self) -> f64 { self.el_reward_increase_percent_precise }
    fn get_equal_to_proxy_bidders(&self) -> &str { &self.equal_to_proxy_bidders }
    fn is_equal_to_proxy_bid(&self) -> bool { self.is_equal_to_proxy_bid }
    fn get_fee_per_block(&self) -> f64 { self.fee_per_block }
}

impl RewardStats for CommitBoostSlotInfo {
    fn get_uid(&self) -> &str { &self.slot_uid }
    fn get_slot(&self) -> &str { &self.slot }
    fn get_block_number(&self) -> &str { &self.block_number }
    fn get_block_hash(&self) -> &str { &self.block_hash }
    fn get_header_start(&self) -> i64 {0 }
    fn get_payload_start(&self) -> i64 { 0}
    fn get_onchain_bid_value(&self) -> f64 { self.onchain_bid_value }
    fn get_el_reward_eth(&self) -> f64 { self.el_reward_increase_eth }
    fn get_el_reward_wei(&self) -> U256 { self.el_reward_increase_wei.clone() }
    fn get_is_proxy_win(&self) -> bool { self.is_proxy_win }
    fn get_is_winning_bid_highest(&self) -> bool { self.is_winning_bid_highest }
    fn get_second_highest_bid_value(&self) -> f64 { self.second_highest_bid_value }
    fn get_second_higher_bid_delivered_relay(&self) -> &str { &self.second_higher_bid_delivered_relay }
    fn get_onchain_bid_delivered_relay(&self) -> &str { &self.onchain_bid_delivered_relay }
    fn get_is_payload_received(&self) -> bool { self.is_payload_received }
    fn get_el_reward_percentage(&self) -> u64 { self.el_reward_increase_percentage }
    fn get_el_reward_precise(&self) -> f64 { self.el_reward_increase_percent_precise }
    fn get_equal_to_proxy_bidders(&self) -> &str { &self.equal_to_proxy_bidders }
    fn is_equal_to_proxy_bid(&self) -> bool { self.is_equal_to_proxy_bid }
    fn get_fee_per_block(&self) -> f64 { self.fee_per_block }
}

pub fn select_final_slot_infos_generic<T>(
    slot_infos: &HashMap<String, HashMap<String, T>>,
) -> HashMap<String, T>
where
    T: RewardStats + Clone + Debug,
{
    let mut final_selected: HashMap<String, T> = HashMap::new();

    for (slot, slot_info_with_uid) in slot_infos {
        debug!("Slot: {}, records: {:?}", slot, slot_info_with_uid);

        let proxy_wins: Vec<_> = slot_info_with_uid
            .values()
            .filter(|si| {
                let valid = si.get_is_proxy_win() && si.get_el_reward_eth().is_finite();
                if si.get_is_proxy_win() {
                    debug!(
                        "[SELECT] slot_uid={} is_proxy_win=true el_reward_increase_eth={} valid={}",
                        si.get_uid(),
                        si.get_el_reward_eth(),
                        valid
                    );
                }
                valid
            })
            .collect();

        let chosen = if !proxy_wins.is_empty() {
            let selected = proxy_wins
                .into_iter()
                .max_by(|a, b| {
                    let cmp = a
                        .get_el_reward_eth()
                        .partial_cmp(&b.get_el_reward_eth())
                        .unwrap_or(std::cmp::Ordering::Equal);
                    if cmp == std::cmp::Ordering::Equal {
                        debug!(
                            "[TIE] Equal el_reward_increase_eth: {} == {}, tie-breaking with slot_uid",
                            a.get_el_reward_eth(),
                            b.get_el_reward_eth()
                        );
                        a.get_uid().cmp(b.get_uid())
                    } else {
                        cmp
                    }
                })
                .unwrap();

            debug!(
                "[CHOSEN] Selected proxy win slot_uid={}, el_reward_increase_eth={}, is_equal_to_proxy_bid={}",
                selected.get_uid(),
                selected.get_el_reward_eth(),
                false // or expose from trait if needed
            );

            Some(selected)
        } else {
            let fallback = slot_info_with_uid.values().next();
            if let Some(f) = fallback {
                debug!(
                    "[CHOSEN] No proxy win, defaulting to first slot_uid={} with block_hash={}",
                    f.get_uid(),
                    f.get_block_hash()
                );
            } else {
                debug!("[CHOSEN] No slot_info entries found at all.");
            }
            fallback
        };

        if let Some(slot_info) = chosen {
            final_selected.insert(slot.clone(), slot_info.clone());
        }
    }

    final_selected
}


pub fn write_csv_generic<T: RewardStats>(
    slot_infos: &HashMap<String, T>,
    folder_path: &str,
    date_str: &str,
    time_str: &str,
) -> IoResult<()> {
    let file_path = format!("{}/slot_infos_{}_{}.csv", folder_path, date_str, time_str);
    let file = File::create(&file_path)?;
    let mut wtr = WriterBuilder::new().has_headers(true).from_writer(file);

    for (_slot, slot_info) in slot_infos {
        let record = SlotInfoWithoutBids {
            slot_uid: slot_info.get_uid(),
            slot: slot_info.get_slot(),
            block_number: slot_info.get_block_number(),
            block_hash: slot_info.get_block_hash(),
            header_start_ms_into_slot: slot_info.get_header_start(),
            payload_start_ms_into_slot: slot_info.get_payload_start(),
            onchain_bid_value: slot_info.get_onchain_bid_value(),
            el_reward_increase_eth: slot_info.get_el_reward_eth(),
            el_reward_increase_wei: slot_info.get_el_reward_wei(),
            is_proxy_win: slot_info.get_is_proxy_win(),
            is_winning_bid_highest: slot_info.get_is_winning_bid_highest(),
            second_highest_bid_value: slot_info.get_second_highest_bid_value(),
            second_higher_bid_delivered_relay: slot_info.get_second_higher_bid_delivered_relay().to_string(),
            onchain_bid_delivered_relay: slot_info.get_onchain_bid_delivered_relay().to_string(),
            is_payload_received: slot_info.get_is_payload_received(),
            el_reward_increase_percentage: slot_info.get_el_reward_percentage(),
            el_reward_increase_percent_precise: slot_info.get_el_reward_precise(),
            equal_to_proxy_bidders: slot_info.get_equal_to_proxy_bidders().to_string(),
            is_equal_to_proxy_bid: slot_info.is_equal_to_proxy_bid(),
            fee_per_block: slot_info.get_fee_per_block(),
        };
        wtr.serialize(record)?;
    }

    wtr.flush()?;
    Ok(())
}

pub fn write_json_generic<K, T>(
    slot_infos: &HashMap<K, T>,
    folder_path: &str,
    date_str: &str,
    time_str: &str,
) -> IoResult<()>
where
    K: std::fmt::Display + Eq + std::hash::Hash + Serialize,
    T: Serialize,
{
    let file_path = format!("{}/slot_infos_{}_{}.json", folder_path, date_str, time_str);
    let mut file = File::create(&file_path)?;
    let json_data = serde_json::to_string_pretty(&slot_infos)?;
    file.write_all(json_data.as_bytes())?;
    Ok(())
}

pub fn write_summary_generic<T: RewardStats>(
    slot_infos: &HashMap<String, T>,
    folder_path: &str,
    date_str: &str,
    time_str: &str,
) -> std::io::Result<()> {
    let total_slots = slot_infos.len();
    let mut slots_won_by_rproxy = 0;
    let mut total_eth = 0.0f64;
    let mut reward_improvement_eth = 0.0f64;

    for info in slot_infos.values() {
        if info.get_is_proxy_win() {
            slots_won_by_rproxy += 1;
            total_eth += info.get_onchain_bid_value();
            reward_improvement_eth += info.get_el_reward_eth();
        }
    }

    let improvement_percentage = if total_eth > 0.0 {
        (reward_improvement_eth / total_eth) * 100.0
    } else {
        0.0
    };

    let summary_path = format!("{}/summary_{}_{}.txt", folder_path, date_str, time_str);
    let mut file = std::fs::File::create(&summary_path)?;
    writeln!(file, "Total Slots           : {}", total_slots)?;
    writeln!(file, "Slots won by Rproxy   : {}", slots_won_by_rproxy)?;
    writeln!(file, "total                 : {:.18} ETH", total_eth)?;
    writeln!(file, "EL reward improvement : {:.18} ETH", reward_improvement_eth)?;
    writeln!(
        file,
        "Improvement percentage: ({:.18} / {:.18}) × 100 ≈ {:.18}%",
        reward_improvement_eth,
        total_eth,
        improvement_percentage
    )?;

    println!("Total Slots           : {}", total_slots);
    println!("Slots won by Rproxy   : {}", slots_won_by_rproxy);
    println!("total                 : {:.18} ETH", total_eth);
    println!("EL reward improvement : {:.18} ETH", reward_improvement_eth);
    println!(
        "Improvement percentage: ({:.18} / {:.18}) × 100 ≈ {:.18}%",
        reward_improvement_eth,
        total_eth,
        improvement_percentage
    );

    Ok(())
}
