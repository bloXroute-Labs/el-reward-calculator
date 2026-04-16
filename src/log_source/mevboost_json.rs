use crate::log_source::common::{get_slot_start_time_utc, is_relay_proxy};
use crate::log_source::types::{Bid, LogEntry};
use crate::{SlotInfo, SlotInfos};
use chrono::{DateTime, SecondsFormat, Utc};
use ethers::types::U256;
use log::debug;
use rust_decimal::prelude::ToPrimitive;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde_json::{self, Deserializer, Value};
use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File};
use std::io::{Result as IoResult, Write};
use url::Url;

/// Metadata collected from a `"best bid"` log entry for a proxy-built block.
///
/// `proxy_url`       — the proxy relay URL that appeared in `relays`.
/// `nonproxy_hosts`  — host names of any non-proxy relays that also appeared
///                     in `relays` (non-empty means the block was co-proposed
///                     and the slot is a tie, not a pure proxy win).
#[derive(Debug, Clone)]
struct BestBidInfo {
    proxy_url: String,
    nonproxy_hosts: Vec<String>,
}

impl BestBidInfo {
    fn has_nonproxy(&self) -> bool {
        !self.nonproxy_hosts.is_empty()
    }
}

pub fn parse_file_content<R: std::io::Read>(reader: R, slot_infos: &mut SlotInfos) {
    // Maps (block_hash, tx_root) -> proxy_url, populated from "best bid" entries
    // where at least one relay in the `relays` field is a relay-proxy.
    //
    // "best bid" lists the relays that actually *proposed* (built) the winning
    // block — unlike "calling getPayload" which is broadcast to ALL relays and
    // therefore cannot identify who built the block.
    //
    // The composite (block_hash, tx_root) key is used instead of block_hash alone
    // for extra precision, since both fields are present in "bid received" entries
    // and in "best bid" entries.
    //
    // MEV-boost >= v1.11.1 dropped url from "bid received" log lines, so
    // bid.relay is empty for all bids.  This map is used post-stream to
    // backfill the relay field only for bids whose block was proxy-built.
    let mut best_bid_proxy_hashes: HashMap<(String, String), BestBidInfo> = HashMap::new();

    let stream = Deserializer::from_reader(reader).into_iter::<Value>();
    for entry in stream {
        match entry {
            Ok(Value::Object(map)) => {
                match serde_json::from_value::<LogEntry>(Value::Object(map)) {
                    Ok(log_entry) => {
                        process_json(&log_entry, slot_infos, &mut best_bid_proxy_hashes);
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
                        Ok(log_entry) => {
                            process_json(&log_entry, slot_infos, &mut best_bid_proxy_hashes)
                        }
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

    // Backfill bid.relay for any bid that still has an empty relay field,
    // using block_hash as the key into best_bid_proxy_hashes.
    backfill_proxy_relay(slot_infos, &best_bid_proxy_hashes);

    // First, drop UIDs that never had any relay-proxy bid at all (diagnostic & cleanup).
    let _ = cleanup_slots_without_proxy(slot_infos);

    // Then compute per-slot classification, applying results strictly per-UID.
    finalize_slot_infos(slot_infos, &best_bid_proxy_hashes);
}

// ----------------------------- Parsing -------------------------------------

fn process_json(
    log_entry: &LogEntry,
    slot_infos: &mut SlotInfos,
    best_bid_proxy_hashes: &mut HashMap<(String, String), BestBidInfo>,
) {
    let slot = log_entry.message.slot.clone();
    let slot_uid = log_entry.message.slotUID.clone();
    // "best bid" entries (and any future entry types) have no slotUID.
    // Don't create a SlotInfo record for them; instead collect the proxy
    // block-hash mapping we need for the backfill step.
    if slot_uid.is_empty() {
        if log_entry.message.method == "getHeader" && log_entry.message.msg == "best bid" {
            let bh = &log_entry.message.blockHash;
            let tx_root = log_entry.message.txRoot.as_deref().unwrap_or("");
            if let Some(relays_str) = &log_entry.message.relays {
                // Scan ALL relays: collect the proxy URL and any non-proxy host
                // names.  Both signals are needed to distinguish a clean proxy
                // win from a co-proposed tie.
                let mut found_proxy_url: Option<String> = None;
                let mut nonproxy_hosts: Vec<String> = Vec::new();
                for relay_url in relays_str.split(',') {
                    let url = relay_url.trim();
                    if url.is_empty() {
                        continue;
                    }
                    if is_relay_proxy(url) {
                        if found_proxy_url.is_none() {
                            found_proxy_url = Some(url.to_string());
                        }
                    } else {
                        nonproxy_hosts.push(host_from(url));
                    }
                }
                if let Some(proxy_url) = found_proxy_url {
                    if !bh.is_empty() {
                        let key = (bh.clone(), tx_root.to_string());
                        best_bid_proxy_hashes
                            .entry(key)
                            .or_insert_with(|| BestBidInfo {
                                proxy_url: proxy_url.clone(),
                                nonproxy_hosts: nonproxy_hosts.clone(),
                            });
                        debug!(
                            "[BEST-BID] proxy proposed block_hash={} tx_root={} relay={} has_nonproxy={}",
                            bh, tx_root, proxy_url, !nonproxy_hosts.is_empty()
                        );
                    }
                }
            }
        }
        return;
    }
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
                let date =
                    DateTime::parse_from_rfc3339(&log_entry.message.time).unwrap_or_else(|_| {
                        panic!(
                            "failed to parse timestamp for slot-{}, timestamp-{}",
                            slot, log_entry.message.time
                        )
                    });
                let date_utc = date.with_timezone(&Utc);
                bid.timestamp = date_utc.timestamp();

                bid.slot = log_entry.message.slot.clone();
                bid.block_hash = log_entry.message.blockHash.clone();
                bid.parent_hash = log_entry.message.parentHash.clone();
                bid.ua = log_entry.message.ua.clone();
                bid.relay = log_entry.message.url.as_deref().unwrap_or("").to_string();
                bid.pubkey = log_entry
                    .message
                    .pubkey
                    .as_deref()
                    .unwrap_or("")
                    .to_string();
                bid.block_number = log_entry
                    .message
                    .blockNumber
                    .map_or(String::new(), |num| num.to_string());
                bid.bid_value = log_entry
                    .message
                    .value
                    .as_deref()
                    .unwrap_or("0.0")
                    .parse::<Decimal>()
                    .unwrap_or(Decimal::ZERO);
                bid.tx_root = log_entry.message.txRoot.clone();

                slot_info.info.bids.push(bid);

                // IMPORTANT: headers NEVER set info.block_hash (not blinded).
                // We only set block_hash from payload ("getPayload") lines.
            }
        }
        "handleGetPayloadV2" | "getPayload" => {
            if log_entry.message.msg == "received payload from relay"
                || log_entry.message.msg == "calling getPayload"
            {
                slot_info.is_payload_received = true;

                // Treat payload blockHash as blinded and add to pending list.
                let bh = log_entry.message.blockHash.clone();
                if !bh.is_empty() && !slot_info.pending_blinded_block_hashes.contains(&bh) {
                    slot_info.pending_blinded_block_hashes.push(bh.clone());
                }

                // Allow payload to set info.block_hash only if currently empty.
                if slot_info.info.block_hash.is_empty() && !bh.is_empty() {
                    slot_info.info.block_hash = bh.clone();
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

fn finalize_slot_infos(
    slot_infos: &mut SlotInfos,
    best_bid_info: &HashMap<(String, String), BestBidInfo>,
) {
    // Work slot-by-slot, then apply strictly per-UID.
    let mut slots: Vec<_> = slot_infos.keys().cloned().collect();
    slots.sort();

    for slot in slots {
        let Some(slot_map) = slot_infos.get_mut(&slot) else {
            continue;
        };

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
            tx_root: String,    // needed for best_bid_info lookup
            value: Decimal,
        }

        // Collect ALL bids across UIDs in THIS slot (own strings).
        let mut all_bids: Vec<BidView> = Vec::new();
        for (_uid, info) in slot_map.iter() {
            for b in &info.info.bids {
                if b.block_hash.is_empty() {
                    continue;
                }
                if b.bid_value <= Decimal::ZERO {
                    continue;
                }
                all_bids.push(BidView {
                    relay: b.relay.clone(),
                    host: host_from(&b.relay),
                    block_hash: b.block_hash.clone(),
                    tx_root: b.tx_root.as_deref().unwrap_or("").to_string(),
                    value: b.bid_value,
                });
            }
        }

        // If slot has no usable bids, zero-out all UIDs; keep strict hash invariant.
        if all_bids.is_empty() {
            for (_uid, info) in slot_map.iter_mut() {
                if !info.info.block_hash.is_empty()
                    && !info
                        .pending_blinded_block_hashes
                        .contains(&info.info.block_hash)
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
        let slot_top_value = all_bids
            .iter()
            .map(|v| v.value)
            .fold(Decimal::ZERO, Decimal::max);

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

        let mut slot_is_loss = !nonproxy_at_top_hosts.is_empty();

        // Ground-truth override using "best bid".relays.
        //
        // Backfill sets bid.relay = proxy_url on EVERY "bid received" entry
        // whose (block_hash, tx_root) matches a proxy-listed "best bid".  When
        // the same block was co-proposed by a non-proxy relay, all copies of
        // that bid look like proxy bids after backfill, so nonproxy_at_top_hosts
        // ends up empty and bid-attribution alone would declare a proxy win.
        //
        // BestBidInfo.nonproxy_hosts captures this signal at parse time:
        // if it is non-empty, the slot is a tie regardless of what per-bid
        // relay fields say.
        if !slot_is_loss && !rproxy_at_top.is_empty() {
            let is_tie_from_best_bid = rproxy_at_top.iter().any(|v| {
                best_bid_info
                    .get(&(v.block_hash.clone(), v.tx_root.clone()))
                    .map_or(false, |i| i.has_nonproxy())
            });
            if is_tie_from_best_bid {
                slot_is_loss = true;
                // Enrich nonproxy_at_top_hosts so that reporting fields
                // (equal_to_proxy_bidders, onchain_bid_delivered_relay) are
                // populated correctly even though backfill masked the non-proxy
                // relay names in the per-bid relay field.
                for v in &rproxy_at_top {
                    if let Some(info) =
                        best_bid_info.get(&(v.block_hash.clone(), v.tx_root.clone()))
                    {
                        for h in &info.nonproxy_hosts {
                            nonproxy_at_top_hosts.insert(h.clone());
                        }
                    }
                }
                debug!(
                    "[TIE-OVERRIDE] slot {} forced to loss via best_bid nonproxy_hosts: {:?}",
                    slot, nonproxy_at_top_hosts
                );
            }
        }

        // Deterministic proxy candidate when proxies win.
        let (
            chosen_hash,
            chosen_proxy_host,
            uplift,
            uplift_wei,
            pct_precise,
            pct_rounded,
            fee_per_block,
        ) = if !slot_is_loss && !rproxy_at_top.is_empty() {
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
                if o != std::cmp::Ordering::Equal {
                    return o;
                }
                a.host.cmp(&b.host)
            });
            let chosen = rproxy_sorted[0];

            let fee = if pct_precise <= dec!(1) {
                dec!(0.0)
            } else if pct_precise <= dec!(5) {
                if uplift >= dec!(0.0015) {
                    dec!(0.0015)
                } else {
                    dec!(0.0)
                }
            } else if pct_precise <= dec!(9) {
                if uplift > dec!(0.003) {
                    dec!(0.003)
                } else if uplift > dec!(0.0015) {
                    dec!(0.0015)
                } else {
                    dec!(0.0)
                }
            } else {
                if uplift > dec!(0.005) {
                    dec!(0.005)
                } else if uplift > dec!(0.003) {
                    dec!(0.003)
                } else if uplift > dec!(0.0015) {
                    dec!(0.0015)
                } else {
                    dec!(0.0)
                }
            };

            (
                chosen.block_hash.clone(),
                chosen.host.clone(),
                uplift,
                uplift_wei,
                pct_precise,
                pct_rounded,
                fee,
            )
        } else {
            (
                String::new(),
                String::new(),
                Decimal::ZERO,
                U256::zero(),
                Decimal::ZERO,
                0u64,
                dec!(0.0),
            )
        };

        // For "loss" reporting
        let eq_nonproxy_join = nonproxy_at_top_hosts
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(", ");
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
                && !info
                    .pending_blinded_block_hashes
                    .contains(&info.info.block_hash)
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
                && info
                    .info
                    .bids
                    .iter()
                    .any(|b| allowed.contains(b.block_hash.as_str()));

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
                    if !info
                        .pending_blinded_block_hashes
                        .contains(&info.info.block_hash)
                    {
                        info.pending_blinded_block_hashes
                            .push(info.info.block_hash.clone());
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
                && !info
                    .pending_blinded_block_hashes
                    .contains(&info.info.block_hash)
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

/// Backfill `bid.relay` for any bid whose relay is currently empty.
///
/// Uses `best_bid_proxy_hashes` ((block_hash, tx_root) → proxy_url) collected
/// from `"best bid"` entries whose `relays` field lists at least one relay-proxy.
///
/// `"best bid".relays` lists only the relays that *built and proposed* the
/// winning block — it is NOT a broadcast list.  This makes it the only
/// reliable source for identifying proxy-built blocks in MEV-boost >= v1.11.1
/// where the `url` field was removed from `"bid received"` log lines.
///
/// The composite (block_hash, tx_root) key adds an extra layer of precision
/// over block_hash alone.
///
/// For older logs where `bid.relay` is already set, this function is a no-op
/// for those bids.
fn backfill_proxy_relay(
    slot_infos: &mut SlotInfos,
    best_bid_proxy_hashes: &HashMap<(String, String), BestBidInfo>,
) {
    if best_bid_proxy_hashes.is_empty() {
        return;
    }
    for slot_map in slot_infos.values_mut() {
        for (slot_uid, slot_info) in slot_map.iter_mut() {
            for bid in slot_info.info.bids.iter_mut() {
                if bid.relay.is_empty() && !bid.block_hash.is_empty() {
                    let tx_root = bid.tx_root.as_deref().unwrap_or("");
                    let key = (bid.block_hash.clone(), tx_root.to_string());
                    if let Some(info) = best_bid_proxy_hashes.get(&key) {
                        debug!(
                            "[BACKFILL] slot_uid: {}, block_hash: {}, tx_root: {} -> relay: {}",
                            slot_uid, bid.block_hash, tx_root, info.proxy_url
                        );
                        bid.relay = info.proxy_url.clone();
                    }
                }
            }
        }
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
            let has_proxy_bid = slot_info
                .info
                .bids
                .iter()
                .any(|bid| is_relay_proxy(&bid.relay));

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
    let summary_path = format!(
        "{}nonproxy_slots_summary_{}_{}.txt",
        dir_path, date_str, time_str
    );

    // JSON dump
    let file = File::create(&json_path)?;
    serde_json::to_writer_pretty(file, &removed)?;
    println!(
        "[Cleanup] Wrote removed no-proxy entries to '{}'",
        json_path
    );

    // Small summary
    let removed_slots_count = removed.len();
    let removed_slot_uids_count: usize = removed.values().map(|m| m.len()).sum();

    let mut sfile = File::create(&summary_path)?;
    writeln!(sfile, "Removed (no relay-proxy bids) summary")?;
    writeln!(sfile, "-----------------------------------")?;
    writeln!(sfile, "Slots checked            : {}", slots_checked)?;
    writeln!(
        sfile,
        "Slots before cleanup     : {}",
        total_slot_count + slots_removed
    )?;
    writeln!(sfile, "Slots removed            : {}", slots_removed)?;
    writeln!(
        sfile,
        "Slot UIDs removed        : {}",
        removed_slot_uids_count
    )?;
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
