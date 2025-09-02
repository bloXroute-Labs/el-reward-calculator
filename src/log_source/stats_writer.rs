use csv::WriterBuilder;
use crate::SlotInfo;
use crate::log_source::types::{CommitBoostSlotInfo, SlotInfoWithoutBids};
use ethers::types::U256;
use serde::Serialize;
use std::cmp::Ordering;
use std::collections::HashMap;
use std::fmt::Debug;
use std::fs::{self, File};
use std::io::{Write, Result as IoResult};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{self, to_string_pretty};
use chrono::Local; // local time for filenames
use url::Url;      // NEW: for host normalization

/// Local timestamp helper
fn local_stamp() -> (String, String) {
    let now = Local::now();
    let date_str = now.format("%Y-%m-%d").to_string();     // e.g. 2025-08-15
    let time_str = now.format("%Hh_%Mm_%Ss").to_string();  // e.g. 17h_25m_05s
    (date_str, time_str)
}

/// Strip \" and \\ then try to parse and return just the host. Fall back to trimmed input.
fn normalize_host_field(raw: &str) -> String {
    let trimmed = raw.trim_matches(|c| c == '"' || c == '\\');
    if let Ok(u) = Url::parse(trimmed) {
        if let Some(h) = u.host_str() {
            return h.to_string();
        }
    }
    // handle strings like host/path or scheme-less leftovers
    if let Some(after_scheme) = trimmed.split("://").nth(1) {
        return after_scheme.split('/').next().unwrap_or(after_scheme).to_string();
    }
    // maybe already just a host
    trimmed.split('/').next().unwrap_or(trimmed).to_string()
}

/// Common stats  for both MEV-Boost and Commit-Boost slot info
pub trait RewardStats: Clone {
    fn get_uid(&self) -> &str;
    fn get_slot(&self) -> &str;
    fn get_block_number(&self) -> &str;
    fn get_block_hash(&self) -> &str;
    fn get_header_start(&self) -> i64;
    fn get_payload_start(&self) -> i64;
    fn get_onchain_bid_value(&self) -> Decimal;
    fn get_el_reward_eth(&self) -> Decimal;
    fn get_el_reward_wei(&self) -> U256;
    fn get_is_proxy_win(&self) -> bool;
    fn get_is_winning_bid_highest(&self) -> bool;
    fn get_second_highest_bid_value(&self) -> Decimal;
    fn get_second_higher_bid_delivered_relay(&self) -> &str;
    fn get_onchain_bid_delivered_relay(&self) -> &str;
    fn get_is_payload_received(&self) -> bool;
    fn get_el_reward_percentage(&self) -> u64;
    fn get_el_reward_precise(&self) -> Decimal;
    fn get_equal_to_proxy_bidders(&self) -> &str;
    fn is_equal_to_proxy_bid(&self) -> bool;
    fn get_fee_per_block(&self) -> Decimal;

    /// RFC3339 slot start time (derived or sourced)
    fn get_slot_start_time(&self) -> &str;
}

impl RewardStats for SlotInfo {
    fn get_uid(&self) -> &str { &self.slot_uid }
    fn get_slot(&self) -> &str { &self.slot }
    fn get_block_number(&self) -> &str { &self.block_number }
    fn get_block_hash(&self) -> &str { &self.info.block_hash }
    fn get_header_start(&self) -> i64 { self.info.header_start_ms_into_slot }
    fn get_payload_start(&self) -> i64 { self.info.payload_start_ms_into_slot }
    fn get_onchain_bid_value(&self) -> Decimal { self.onchain_bid_value }
    fn get_el_reward_eth(&self) -> Decimal { self.el_reward_increase_eth }
    fn get_el_reward_wei(&self) -> U256 { self.el_reward_increase_wei.clone() }
    fn get_is_proxy_win(&self) -> bool { self.is_proxy_win }
    fn get_is_winning_bid_highest(&self) -> bool { self.is_winning_bid_highest }
    fn get_second_highest_bid_value(&self) -> Decimal { self.second_highest_bid_value }
    fn get_second_higher_bid_delivered_relay(&self) -> &str { &self.second_higher_bid_delivered_relay }
    fn get_onchain_bid_delivered_relay(&self) -> &str { &self.onchain_bid_delivered_relay }
    fn get_is_payload_received(&self) -> bool { self.is_payload_received }
    fn get_el_reward_percentage(&self) -> u64 { self.el_reward_increase_percentage }
    fn get_el_reward_precise(&self) -> Decimal { self.el_reward_increase_percent_precise }
    fn get_equal_to_proxy_bidders(&self) -> &str { &self.equal_to_proxy_bidders }
    fn is_equal_to_proxy_bid(&self) -> bool { self.is_equal_to_proxy_bid }
    fn get_fee_per_block(&self) -> Decimal { self.fee_per_block }
    fn get_slot_start_time(&self) -> &str { &self.time }
}

impl RewardStats for CommitBoostSlotInfo {
    fn get_uid(&self) -> &str { &self.slot_uid }
    fn get_slot(&self) -> &str { &self.slot }
    fn get_block_number(&self) -> &str { &self.block_number }
    fn get_block_hash(&self) -> &str { &self.block_hash }
    fn get_header_start(&self) -> i64 { 0 }
    fn get_payload_start(&self) -> i64 { 0 }
    fn get_onchain_bid_value(&self) -> Decimal { self.onchain_bid_value }
    fn get_el_reward_eth(&self) -> Decimal { self.el_reward_increase_eth }
    fn get_el_reward_wei(&self) -> U256 { self.el_reward_increase_wei.clone() }
    fn get_is_proxy_win(&self) -> bool { self.is_proxy_win }
    fn get_is_winning_bid_highest(&self) -> bool { self.is_winning_bid_highest }
    fn get_second_highest_bid_value(&self) -> Decimal { self.second_highest_bid_value }
    fn get_second_higher_bid_delivered_relay(&self) -> &str { &self.second_higher_bid_delivered_relay }
    fn get_onchain_bid_delivered_relay(&self) -> &str { &self.onchain_bid_delivered_relay }
    fn get_is_payload_received(&self) -> bool { self.is_payload_received }
    fn get_el_reward_percentage(&self) -> u64 { self.el_reward_increase_percentage }
    fn get_el_reward_precise(&self) -> Decimal { self.el_reward_increase_percent_precise }
    fn get_equal_to_proxy_bidders(&self) -> &str { &self.equal_to_proxy_bidders }
    fn is_equal_to_proxy_bid(&self) -> bool { self.is_equal_to_proxy_bid }
    fn get_fee_per_block(&self) -> Decimal { self.fee_per_block }
    fn get_slot_start_time(&self) -> &str { &self.time }
}

/// Select exactly one UID per slot deterministically:
///  0) filter out non-resolved / zero cases,
///  1) prefer proxy win with positive uplift,
///  2) higher uplift,
///  3) prefer payload received,
///  4) earlier payload start,
///  5) earlier header start,
///  6) tie-break by (block_hash, uid).
pub fn select_final_slot_infos_generic<T>(
    slot_infos: &HashMap<String, HashMap<String, T>>,
) -> HashMap<String, T>
where
    T: RewardStats + Clone + Debug,
{
    let mut final_selected: HashMap<String, T> = HashMap::new();

    for (slot, uid_map) in slot_infos {
        let mut candidates: Vec<&T> = uid_map.values().collect();

        // 0) keep only meaningful, resolved entries
        candidates.retain(|c|
            !c.get_block_hash().is_empty() &&
            c.get_onchain_bid_value() > Decimal::ZERO
        );

        if candidates.is_empty() {
            continue;
        }

        candidates.sort_by(|a, b| {
            // (1) proxy win + positive uplift
            let a_pref = a.get_is_proxy_win() && a.get_el_reward_eth() > Decimal::ZERO;
            let b_pref = b.get_is_proxy_win() && b.get_el_reward_eth() > Decimal::ZERO;
            if a_pref != b_pref {
                return if b_pref { Ordering::Greater } else { Ordering::Less };
            }

            // (2) higher uplift first
            match b.get_el_reward_eth()
                .partial_cmp(&a.get_el_reward_eth())
                .unwrap_or(Ordering::Equal)
            {
                Ordering::Less    => return Ordering::Less,
                Ordering::Greater => return Ordering::Greater,
                Ordering::Equal   => {}
            }

            // (3) payload received preferred
            let ord = b.get_is_payload_received().cmp(&a.get_is_payload_received());
            if ord != Ordering::Equal { return ord; }

            // (4) earlier payload start
            let ord = a.get_payload_start().cmp(&b.get_payload_start());
            if ord != Ordering::Equal { return ord; }

            // (5) earlier header start
            let ord = a.get_header_start().cmp(&b.get_header_start());
            if ord != Ordering::Equal { return ord; }

            // (6) stable tie-breaks
            let ord = a.get_block_hash().cmp(b.get_block_hash());
            if ord != Ordering::Equal { return ord; }
            a.get_uid().cmp(b.get_uid())
        });

        if let Some(best) = candidates.first() {
            final_selected.insert(slot.clone(), (*best).clone());
        }
    }

    final_selected
}

/// Collapse any flat map keyed by slot_uid into one record per *slot*.
/// (Kept for compatibility—safe even when map is already per-slot.)
pub fn coalesce_by_slot_generic<T>(
    selected_infos: &HashMap<String, T>,
) -> HashMap<String, T>
where
    T: RewardStats + Clone + Debug,
{
    let mut per_slot: HashMap<String, T> = HashMap::new();

    for info in selected_infos.values() {
        let slot_key = info.get_slot().to_string();

        per_slot
            .entry(slot_key)
            .and_modify(|existing| {
                let existing_is_valid = existing.get_is_proxy_win() && existing.get_el_reward_eth() > Decimal::ZERO;
                let incoming_is_valid = info.get_is_proxy_win() && info.get_el_reward_eth() > Decimal::ZERO;

                let take_incoming = match (existing_is_valid, incoming_is_valid) {
                    (false, true) => true,
                    (true, false) => false,
                    (true, true) => match info.get_el_reward_eth().partial_cmp(&existing.get_el_reward_eth()).unwrap_or(Ordering::Equal) {
                        Ordering::Greater => true,
                        Ordering::Equal => {
                            // add stable tie-break on block_hash too if you want:
                            let h = info.get_block_hash().cmp(existing.get_block_hash());
                            if h != Ordering::Equal {
                                h == Ordering::Less
                            } else {
                                info.get_uid() < existing.get_uid()
                            }
                        }
                        Ordering::Less => false,
                    },
                    (false, false) => false,
                };

                if take_incoming {
                    *existing = info.clone();
                }
            })
            .or_insert_with(|| info.clone());
    }

    per_slot
}

/// CSV writer (uses local time for filename)
/// Expects **per-slot** input (one entry per slot).
pub fn write_csv_generic<T: RewardStats>(
    slot_infos: &HashMap<String, T>,
    folder_path: &str,
    _date_str: &str,
    _time_str: &str,
) -> IoResult<()> {
    let (date_str, time_str) = local_stamp();
    let file_path = format!("{}/slot_infos_{}_{}.csv", folder_path, date_str, time_str);
    let file = File::create(&file_path)?;
    let mut wtr = WriterBuilder::new().has_headers(true).from_writer(file);

    for (_slot_key, slot_info) in slot_infos {
        // Clean relay fields for CSV readability
        let delivered = normalize_host_field(slot_info.get_onchain_bid_delivered_relay());
        let second_delivered = normalize_host_field(slot_info.get_second_higher_bid_delivered_relay());

        let record = SlotInfoWithoutBids {
            time: slot_info.get_slot_start_time(),
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
            second_higher_bid_delivered_relay: second_delivered,
            onchain_bid_delivered_relay: delivered,
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

/// CSV writer that guarantees one row per *slot*
pub fn write_csv_per_slot_generic<T: RewardStats + Clone + Debug>(
    selected_infos: &HashMap<String, T>,
    folder_path: &str,
    _date_str: &str,
    _time_str: &str,
) -> IoResult<()> {
    let per_slot = coalesce_by_slot_generic(selected_infos);
    write_csv_generic(&per_slot, folder_path, "", "")
}

pub fn write_json_generic<K, T>(
    slot_infos: &HashMap<K, T>,
    folder_path: &str,
    _date_str: &str,
    _time_str: &str,
) -> IoResult<()>
where
    K: std::fmt::Display + Eq + std::hash::Hash + Serialize,
    T: Serialize,
{
    let (date_str, time_str) = local_stamp();
    let file_path = format!("{}/slot_infos_{}_{}.json", folder_path, date_str, time_str);
    let mut file = File::create(&file_path)?;
    let json_data = serde_json::to_string_pretty(&slot_infos)?;
    file.write_all(json_data.as_bytes())?;
    Ok(())
}

/// Writes the summary (per-slot view) and logs skipped infos to JSON.
pub fn write_summary_generic<T: RewardStats + std::fmt::Debug + Serialize>(
    selected_infos: &HashMap<String, T>,
    folder_path: &str,
    _date_str: &str,
    _time_str: &str,
    _all_infos: &[T],
    skipped_infos:  &HashMap<String, Vec<(String, T, Vec<&'static str>)>>,
) -> std::io::Result<()> {
    use std::io::Write;

    let (date_str, time_str) = local_stamp();

    // Per-slot representatives
    let per_slot = coalesce_by_slot_generic(selected_infos);

    // Counts
    let per_slot_set: std::collections::HashSet<String> =
        per_slot.values().map(|i| i.get_slot().to_string()).collect();
    let skipped_slot_set: std::collections::HashSet<String> =
        skipped_infos.keys().cloned().collect();

    let mut union_slots = per_slot_set.clone();
    union_slots.extend(skipped_slot_set.iter().cloned());
    let total_slots_parsed = union_slots.len();
    let skipped_slots_count = skipped_slot_set.difference(&per_slot_set).count();
    let total_slots_considered = per_slot.len();
    let total_slot_uids = selected_infos.len();

    // Metrics (per-slot stable totals)
    let total_eth_overall: Decimal = per_slot.values().map(|i| i.get_onchain_bid_value()).sum();

    let mut slots_won_by_rproxy = 0usize;
    let mut total_eth_rproxy = Decimal::ZERO;
    let mut reward_improvement_eth = Decimal::ZERO;
    let mut total_fee_per_block_eth = Decimal::ZERO;

    println!("Total slot_infos parsed_before (slot_uids): {}", total_slot_uids);

    let mut sorted_infos: Vec<_> = per_slot.values().collect();
    sorted_infos.sort_by(|a, b| a.get_uid().cmp(&b.get_uid()));

    let checksum_per_slot: Decimal = sorted_infos.iter().map(|i| i.get_onchain_bid_value()).sum();
    println!("Total ETH Checksum (per-slot): {:.18}", checksum_per_slot);

    for info in sorted_infos {
        if info.get_is_proxy_win() {
            slots_won_by_rproxy += 1;
            total_eth_rproxy += info.get_onchain_bid_value();
            reward_improvement_eth += info.get_el_reward_eth();
            total_fee_per_block_eth += info.get_fee_per_block();
        }
    }

    let improvement_percentage = if total_eth_rproxy > Decimal::ZERO {
        (reward_improvement_eth / total_eth_rproxy) * dec!(100.0)
    } else {
        Decimal::ZERO
    };

    let owed_to_blxr = reward_improvement_eth * dec!(0.5);

    let summary_path = format!("{}/summary_{}_{}.txt", folder_path, date_str, time_str);
    let mut file = File::create(&summary_path)?;

    writeln!(file, "--------------------------------------------------------")?;
    writeln!(file, "Total slots parsed : {}", total_slots_parsed)?;
    writeln!(file, "Total Slot UIDs(single slot contain multiple UIDs): {}", total_slot_uids)?;
    writeln!(file, "Skipped slot : {} (refer skipped slots file for reason)", skipped_slots_count)?;
    writeln!(file, "")?;

    writeln!(file, "--------------------------------------------------------")?;
    writeln!(file, "Total Slots(considered for calculation)            : {}", total_slots_considered)?;
    writeln!(file, "total eth overall      : {:.18} ETH", total_eth_overall)?;
    writeln!(file, "Slots won by Rproxy    : {}", slots_won_by_rproxy)?;
    writeln!(file, "total eth (Rproxy slots): {:.18} ETH", total_eth_rproxy)?;
    writeln!(file, "EL reward improvement  : {:.18} ETH", reward_improvement_eth)?;
    writeln!(
        file,
        "Improvement percentage : ({:.18} / {:.18}) × 100 ≈ {:.18}%",
        reward_improvement_eth,
        total_eth_rproxy,
        improvement_percentage
    )?;
    writeln!(file, "50% Owed to BLXR       : {:.18} ETH", owed_to_blxr)?;
    writeln!(file, "Total fee per block    : {:.18} ETH", total_fee_per_block_eth)?;
    writeln!(file, "--------------------------------------------------------")?;

    // Mirror to stdout
    println!("-----------------------------------------------------------");
    println!("Total slots parsed : {}", total_slots_parsed);
    println!("Total Slot UIDs(single slot contain multiple UIDs): {}", total_slot_uids);
    println!("Skipped slots : {} (refer skipped slots file for reason)", skipped_slots_count);
    println!("-----------------------------------------------------------");

    println!("Total Slots(considered for calculation): {}", total_slots_considered);
    println!("total eth overall         : {:.18} ETH", total_eth_overall);
    println!("Slots won by Rproxy       : {}", slots_won_by_rproxy);
    println!("total eth (Rproxy slots)  : {:.18} ETH", total_eth_rproxy);
    println!("EL reward improvement     : {:.18} ETH", reward_improvement_eth);
    println!(
        "Improvement percentage    : ({:.18} / {:.18}) × 100 ≈ {:.18}%",
        reward_improvement_eth, total_eth_rproxy, improvement_percentage
    );
    println!("50% Owed to BLXR         : {:.18} ETH", owed_to_blxr);
    println!("Total fee per block       : {:.18} ETH", total_fee_per_block_eth);
    println!("-----------------------------------------------------------");

    // Skipped infos
    let skipped_dir = format!("{}/skipped", folder_path);
    fs::create_dir_all(&skipped_dir)?;
    let skipped_path = format!("{}/skipped_out_{}_{}.json", skipped_dir, date_str, time_str);
    let mut skipped_file = File::create(&skipped_path)?;
    log_skipped_infos(skipped_infos, &mut skipped_file)?;

    Ok(())
}

/// Logs skipped slot infos fully in JSON format
pub fn log_skipped_infos<T: std::fmt::Debug + Serialize>(
    skipped_infos: &HashMap<String, Vec<(String, T, Vec<&'static str>)>>,
    file: &mut File,
) -> std::io::Result<()> {
    for (slot, entries) in skipped_infos {
        for (slot_uid, info, reasons) in entries {
            let json_entry = serde_json::json!({
                "slot": slot,
                "slot_uid": slot_uid,
                "reasons": reasons,
                "info": info
            });
            writeln!(file, "{}", to_string_pretty(&json_entry).unwrap())?;
        }
    }
    Ok(())
}
