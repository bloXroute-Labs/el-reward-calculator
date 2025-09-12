//! Offline-first proposer comparison that uses an exact timestamp from a local JSON.
//!
//! Behavior:
//! - Reads a hard-coded JSON (array or NDJSON) with one-or-many entries per slot.
//! - For each slot we’re asked to check, we REQUIRE `proposer_send_timestamp_ms` in the JSON.
//!   No slot-level fallback is used.
//! - Queries one-or-many proposer endpoints, filters to rows whose
//!   `proposer_send_timestamp_ms` equals the JSON timestamp, then computes uplift,
//!   equal-to-proxy logic, fee-per-block, etc.
//! - Writes a JSON of per-slot analyses and a CSV comparing calculator vs proposer.
//!
//! Call from your pipeline AFTER writing the normal CSV:
//!     proposer_live::run_proposer_compare_from_json_and_write(&selected_infos_map, &folder_path)?;
//!
use anyhow::{anyhow, Context, Result};
use chrono::Local;
use csv::WriterBuilder;
use futures::stream::{self, StreamExt};
use log::warn;
use num_bigint::BigInt;
use num_traits::{Signed, Zero};
use reqwest::StatusCode;
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::{
    cmp::Ordering,
    collections::{BTreeSet, HashMap, HashSet},
    fs::{self, File},
    io::{BufRead, BufReader, Read},
    path::PathBuf,
    str::FromStr,
    time::Duration,
};
use tokio::time::sleep;
use url::Url;

// ---- CHANGE ME: hard-coded path to the input JSON with proposer timestamps
// const INPUT_JSON_PATH: &str = "/Users/bhaki/Documents/BloXroute/logs/slot_stats/2025/August/12265295.json";
const INPUT_JSON_PATH: &str = "/Users/bhaki/Documents/BloXroute/logs/slot_stats/2025/August/kraken_august_slot_stats.json";

// ============== Types read from the local JSON ==============

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
struct InputEntry {
    slot: u64,

    // exact timestamp to use for matching (REQUIRED for the slot to be considered)
    #[serde(default)]
    header_start_time_unix_ms: Option<String>,

    // optional: on-chain fields (used for equality checks / details)
    #[serde(default)]
    win: Option<String>,
    #[serde(default)]
    node_id: Option<String>,
    #[serde(default)]
    header_delivered_block_hash: Option<String>,
    #[serde(default)]
    payload_delivered_block_hash: Option<String>,
    #[serde(default)]
    header_block_value: Option<f64>,
    #[serde(default)]
    payload_block_value: Option<f64>,
}

#[derive(Debug, Deserialize, Clone)]
#[serde(rename_all = "snake_case")]
struct RelayRow {
    #[serde(default)]
    header_request_id: Option<String>,
    #[serde(default)]
    slot: Option<String>,
    #[serde(default)]
    parent_hash: Option<String>,
    #[serde(default)]
    block_hash: Option<String>,
    #[serde(default)]
    builder_pubkey: Option<String>,
    #[serde(default)]
    proposer_pubkey: Option<String>,
    #[serde(default)]
    proposer_fee_recipient: Option<String>,
    #[serde(default)]
    value: Option<String>, // wei as string
    #[serde(default)]
    block_number: Option<String>,
    #[serde(default)]
    value_eth: Option<String>,
    #[serde(default)]
    proposer_send_timestamp_ms: Option<String>,
    #[serde(default)]
    extra_data: Option<String>,
    #[serde(default, alias = "nodeId", alias = "nodeID", alias = "node_id")]
    node_id: Option<String>,
}

#[derive(Clone, Debug)]
struct Endpoint {
    url: String,
    api_key: Option<String>,
}
impl Endpoint {
    fn label(&self) -> String {
        Url::parse(&self.url)
            .ok()
            .and_then(|u| u.host_str().map(|h| h.to_string()))
            .unwrap_or_else(|| self.url.clone())
    }
}

#[derive(Clone, Debug)]
struct RowWithSrc {
    row: RelayRow,
    raw: Value,
    src: String,
}

#[derive(Debug, Deserialize, Clone)]
struct UltrasoundAdjustment {
    #[serde(default)]
    adjusted_block_hash: Option<String>,
    #[serde(default)]
    adjusted_value: Option<String>, // wei
    #[serde(default)]
    block_number: Option<u64>,
    #[serde(default)]
    builder_pubkey: Option<String>,
    #[serde(default)]
    delta: Option<String>,
    #[serde(default)]
    submitted_block_hash: Option<String>,
    #[serde(default)]
    submitted_received_at: Option<String>,
    #[serde(default)]
    submitted_value: Option<String>, // wei
}
#[derive(Debug, Deserialize, Clone)]
struct UltrasoundAdjustmentEnvelope {
    data: Vec<UltrasoundAdjustment>,
}

// ============== Public API ==============

use crate::log_source::stats_writer::RewardStats;

/// Main entrypoint (sync): read JSON, analyze each slot by exact timestamp, write JSON + comparison CSV.
pub fn run_proposer_compare_from_json_and_write<T: RewardStats>(
    per_slot_selected: &HashMap<String, T>,
    folder_path: &str,
) -> Result<()> {
    // collect the set of slots we will analyze
    let mut slots: Vec<u64> = Vec::new();
    for (slot_str,slot_info) in per_slot_selected.iter() {
        warn!("proposer_live: raw slot key = {:?}", slot_str.trim());
        if slot_info.get_is_proxy_win(){
            if let Ok(k) = slot_str.parse::<u64>() {
                slots.push(k);
            }
        }
    }
    if slots.is_empty() {
        warn!("proposer_live: no slots to analyze");
        return Ok(());
    }

    // read inputs and map slot -> best input (must have header_start_time_unix_ms)
    let entries_by_slot = read_input_entries_by_slot(INPUT_JSON_PATH.into())?;
    let mut slot_to_entry: HashMap<u64, InputEntry> = HashMap::new();
    for slot in &slots {
        if let Some(mut vv) = entries_by_slot.get(slot).cloned() {
            // keep only entries with header_start_time_unix_ms
            vv.retain(|e| {
                e.header_start_time_unix_ms
                    .as_deref()
                    .map(|s| !s.trim().is_empty())
                    .unwrap_or(false)
            });
            if vv.is_empty() {
                warn!(
                    "proposer_live: slot {} has no header_start_time_unix_ms in {}",
                    slot, INPUT_JSON_PATH
                );
                continue;
            }
            // pick earliest timestamp if many
            // vv.sort_by(|a, b| a.header_start_time_unix_ms.cmp(&b.header_start_time_unix_ms));
            slot_to_entry.insert(*slot, vv[0].clone());
        } else {
            warn!(
                "proposer_live: no input found for slot {} in {}",
                slot, INPUT_JSON_PATH
            );
        }
    }
    if slot_to_entry.is_empty() {
        warn!("proposer_live: nothing to analyze (no slots had header_start_time_unix_ms)");
        return Ok(());
    }

    // build a Tokio runtime (since the rest of your pipeline is sync)
    let rt = tokio::runtime::Runtime::new()?;
    let (analyses, _relays_by_ts) = rt.block_on(async {
        let endpoints = resolve_endpoints_from_env();
        let client = reqwest::Client::builder()
            .use_rustls_tls()
            .gzip(true)
            .connect_timeout(Duration::from_secs(10))
            .timeout(Duration::from_secs(20))
            .build()
            .expect("client");

        // concurrent analyze
        let outs = stream::iter(slot_to_entry.into_iter().map(|(_slot, entry)| {
            let client = &client;
            let eps = endpoints.clone();
            async move { analyze_one_slot_exact_ts(client, &entry, &eps).await }
        }))
        .buffer_unordered(12)
        .collect::<Vec<_>>()
        .await;

        // merge into maps
        let mut analyses: HashMap<u64, SlotAnalysis> = HashMap::new();
        let mut relays_by_ts_all: HashMap<u64, HashMap<String, HashMap<String, Value>>> =
            HashMap::new();

        for r in outs {
            match r {
                Ok((sa, per_ts)) => {
                    relays_by_ts_all.insert(sa.slot, per_ts);
                    analyses.insert(sa.slot, sa);
                }
                Err(e) => warn!("proposer_live: slot analysis error: {e:#}"),
            }
        }
        (analyses, relays_by_ts_all)
    });

    if analyses.is_empty() {
        warn!("proposer_live: analyses empty; skipping file writes");
        return Ok(());
    }

    // write JSON with all analyses
    let (date_str, time_str) = local_stamp();
    fs::create_dir_all(folder_path)?;
    let json_path = format!(
        "{}/proposer_compare_{}_{}.json",
        folder_path.trim_end_matches('/'),
        date_str,
        time_str
    );
    let jf = File::create(&json_path)?;
    serde_json::to_writer_pretty(jf, &analyses)?;
    eprintln!("wrote {}", json_path);

    // write comparison CSV vs calculator outputs
    let csv_path = format!(
        "{}/proposer_compare_{}_{}.csv",
        folder_path.trim_end_matches('/'),
        date_str,
        time_str
    );
    let cf = File::create(&csv_path)?;
    let mut w = WriterBuilder::new().has_headers(true).from_writer(cf);

    // header
    // header
    w.write_record(&[
        "slot",
        "calc_onchain_bid_eth",
        "prop_onchain_bid_eth",
        "calc_el_reward_eth",
        "prop_el_reward_eth",
        "calc_fee_per_block_eth",
        "prop_fee_per_block_eth",
        "calc_proxy_win",
        "prop_proxy_win",
        "calc_equal_to_proxy_bid",
        "prop_equal_to_proxy_bid",
        "calc_block_hash",
        "prop_block_hash",
        "calc_delivered_host",
        "prop_delivered_host",
        "onchain_bid_abs_diff",
        "el_reward_abs_diff",
        "has_el_reward_diff",
    ])?;


    for (slot_str, calc) in per_slot_selected {
        let slot_u64 = match slot_str.parse::<u64>() {
            Ok(s) => s,
            Err(_) => continue,
        };
        let prop = match analyses.get(&slot_u64) {
            Some(p) => p,
            None => continue,
        };

        let calc_onchain = truncate3_decimal(calc.get_onchain_bid_value());
        let prop_onchain = truncate3_decimal(dec_from_opt_str(prop.onchain_bid_value.as_deref()));

        let calc_uplift = truncate3_decimal(calc.get_el_reward_eth());
        let prop_uplift = truncate3_decimal(dec_from_opt_str(prop.el_reward_increase_eth.as_deref()));

        let calc_fee = truncate3_decimal(calc.get_fee_per_block());
        let prop_fee = truncate3_decimal(prop.fee_per_block);

        let calc_proxy_win = calc.get_is_proxy_win();
        let prop_proxy_win = prop.is_proxy_win;

        let calc_equal = calc.is_equal_to_proxy_bid();
        let prop_equal = prop.is_equal_to_proxy_bid;

        let calc_hash = calc.get_block_hash().to_string();
        let prop_hash = prop.onchain_block_hash.clone().unwrap_or_default();

        let calc_host = calc.get_onchain_bid_delivered_relay().to_string();
        let prop_host = prop
            .onchain_bid_delivered_relay
            .clone()
            .unwrap_or_default();

        let onchain_diff = (calc_onchain - prop_onchain).abs();
        let uplift_diff = (calc_uplift - prop_uplift).abs();
        let has_el_reward_diff = uplift_diff > Decimal::ZERO;

        // Build a row of owned Strings to satisfy csv's trait bounds cleanly
        let row: Vec<String> = vec![
            slot_str.clone(),
            fmt18_truncate3(calc_onchain),
            fmt18_truncate3(prop_onchain),
            fmt18_truncate3(calc_uplift),
            fmt18_truncate3(prop_uplift),
            fmt18_truncate3(calc_fee),
            fmt18_truncate3(prop_fee),
            calc_proxy_win.to_string(),
            prop_proxy_win.to_string(),
            calc_equal.to_string(),
            prop_equal.to_string(),
            calc_hash,
            prop_hash,
            calc_host,
            prop_host,
            fmt18_truncate3(onchain_diff),
            fmt18_truncate3(uplift_diff),
            has_el_reward_diff.to_string(),
        ];
        w.write_record(&row)?;
    }
    w.flush()?;
    eprintln!("wrote {}", csv_path);

    // (optional) if you want the raw per-ts rows persisted later, you already have `relays_by_ts` above

    Ok(())
}

// ============== Per-slot analysis (exact TS only) ==============

#[derive(Debug, Serialize, Clone)]
struct SlotAnalysis {
    slot: u64,
    block_number: Option<String>,
    proposer_pubkey: Option<String>,
    parent_hash: Option<String>,
    builder_pubkey: Option<String>,
    proposer_fee_recipient: Option<String>,
    onchain_block_hash: Option<String>,

    // summary fields
    is_proxy_win: bool,
    is_winning_bid_highest: bool,

    is_equal_to_proxy_bid: bool,
    equal_to_proxy_bidders: String,

    onchain_bid_value: Option<String>,
    second_highest_bid_value: Option<String>,
    second_highest_bid_submitted_value: Option<String>,
    second_highest_bid_adjusted: bool,

    onchain_bid_delivered_relay: Option<String>,
    second_higher_bid_delivered_relay: Option<String>,

    el_reward_increase_wei: Option<String>,
    el_reward_increase_eth: Option<String>,

    el_reward_increase_percentage: Option<i64>,
    el_reward_increase_percent_precise: Option<String>,

    matched_by_timestamp: usize,
    delivered_by_any_endpoint: bool,
    delivered_in_ts_matched: bool,
    input_value_eth: Option<String>,
    max_endpoint_value_eth: Option<String>,
    el_reward_improvement_eth: Option<String>,
    endpoints_used: Vec<String>,

    fee_per_block: Decimal,

    relay_proxy_win_no_bids_from_relays: bool,
    rproxy_input_win_marked_lost_due_equal: bool,
}

async fn analyze_one_slot_exact_ts(
    client: &reqwest::Client,
    entry: &InputEntry,
    endpoints: &[Endpoint],
) -> Result<(SlotAnalysis, HashMap<String, HashMap<String, Value>>)> {
    let slot = entry.slot;
    let ts = entry
        .header_start_time_unix_ms
        .clone()
        .ok_or_else(|| anyhow!("slot {} missing header_start_time_unix_ms", slot))?;

    // fetch all endpoints concurrently
    let fetches = stream::iter(endpoints.iter().cloned().map(|ep| async move {
        let rows = fetch_rows(client, &ep, slot).await;
        (ep, rows)
    }))
    .buffer_unordered(endpoints.len().max(1))
    .collect::<Vec<_>>()
    .await;

    let mut endpoints_used = vec![];
    let mut all_rows_with_src: Vec<RowWithSrc> = vec![];

    for (ep, res) in fetches {
        let label = ep.label();
        match res {
            Ok(mut rows) => {
                rows.retain(|(r, _)| row_slot_eq(r, slot));
                endpoints_used.push(label.clone());
                for (r, raw) in rows {
                    all_rows_with_src.push(RowWithSrc {
                        row: r,
                        raw,
                        src: label.clone(),
                    });
                }
            }
            Err(e) => {
                endpoints_used.push(label.clone());
                warn!("fetch failed for {} (slot {}): {}", ep.url, slot, e);
            }
        }
    }

    // on-chain hash & value from input
    let onchain_hash = onchain_block_hash(entry);
    let onchain_value_eth = entry
        .header_block_value
        .or(entry.payload_block_value)
        .map(|f| format!("{:.18}", f));
    let onchain_value_wei = onchain_value_eth.as_deref().map(eth_str_to_wei);

    // Filter EXACT timestamp rows, drop proxy node_ids, and drop bloXroute rows equal-to-onchain
    let ts_matched_with_src: Vec<RowWithSrc> = all_rows_with_src
        .into_iter()
        .filter(|r| r.row.proposer_send_timestamp_ms.as_deref() == Some(ts.as_str()))
        .filter(|r| !has_proxy_node_id(r))
        .filter(|r| {
            if !is_bloxroute_label(&r.src) {
                return true;
            }
            let Some(h) = onchain_hash.as_deref() else {
                return true;
            };
            let bh_opt = r.row.block_hash.as_deref().map(|s| s.trim().to_lowercase());
            match bh_opt {
                Some(bh) if bh == h.trim().to_lowercase() => false,
                _ => true,
            }
        })
        .collect();

    let matched_by_timestamp = {
        let mut uniq = HashSet::new();
        for r in &ts_matched_with_src {
            uniq.insert(r.src.clone());
        }
        uniq.len()
    };

    // relays_by_timestamp (only that exact ts)
    let mut relays_by_ts: HashMap<String, HashMap<String, Value>> = HashMap::new();
    let mut inner: HashMap<String, Value> = HashMap::new();
    for r in &ts_matched_with_src {
        inner.insert(r.src.clone(), r.raw.clone());
    }
    relays_by_ts.insert(ts.clone(), inner);

    // delivery flags (strict to ts-matched set)
    let delivered_any = block_hash_in_rows(onchain_hash.as_deref(), &ts_matched_with_src);
    let delivered_in_ts_matched = delivered_any;

    // Ultrasound adjustments (only if present in the ts-matched set)
    let mut ref_rows = ts_matched_with_src.clone();
    let mut us_adjustments: Vec<UltrasoundAdjustment> = Vec::new();
    if ref_rows.iter().any(|r| is_ultrasound_label(&r.src)) {
        if let Ok(rows) = fetch_ultrasound_adjustments(client, slot).await {
            us_adjustments = rows;
        }
        // pre-apply adjustments
        for r in &mut ref_rows {
            if is_ultrasound_label(&r.src) {
                if let Some(bh) = r.row.block_hash.clone() {
                    if let Some(adj) = us_adjustments.iter().find(|a| {
                        let sbh = a.submitted_block_hash.as_deref().unwrap_or("");
                        let abh = a.adjusted_block_hash.as_deref().unwrap_or("");
                        sbh.eq_ignore_ascii_case(&bh) || abh.eq_ignore_ascii_case(&bh)
                    }) {
                        if let Some(adj_val_s) = adj.adjusted_value.as_deref() {
                            r.row.value = Some(adj_val_s.to_string());
                            if let Some(obj) = r.raw.as_object_mut() {
                                obj.insert("value".to_string(), Value::String(adj_val_s.to_string()));
                                if let Some(sub) = adj.submitted_value.as_deref() {
                                    obj.insert(
                                        "submitted_value".to_string(),
                                        Value::String(sub.to_string()),
                                    );
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    // competitors (value used may be adjusted)
    let mut competitor_values: Vec<(BigInt, String, Option<String>)> = vec![];
    for rs in &ref_rows {
        if let Some(vs) = rs.row.value.as_deref() {
            if let Some(v) = parse_bigint(vs) {
                competitor_values.push((v, rs.src.clone(), rs.row.block_hash.clone()));
            }
        }
    }

    // highest competitor value
    let mut max_competitor: Option<(BigInt, String)> = None;
    for (v, src, _) in &competitor_values {
        if max_competitor.as_ref().map_or(true, |(m, _)| v > m) {
            max_competitor = Some((v.clone(), src.clone()));
        }
    }

    // equal-to-proxy by block_hash inside EXACT-TS set
    let mut equal_relays_ts = BTreeSet::new();
    if let Some(ref h) = onchain_hash {
        let needle = h.trim().to_lowercase();
        for rs in &ref_rows {
            if let Some(bh) = rs.row.block_hash.as_deref() {
                if bh.trim().eq_ignore_ascii_case(&needle) {
                    equal_relays_ts.insert(rs.src.clone());
                }
            }
        }
    }
    let is_equal_to_proxy_bid = !equal_relays_ts.is_empty();
    let equal_to_proxy_bidders = if is_equal_to_proxy_bid {
        equal_relays_ts
            .iter()
            .cloned()
            .collect::<Vec<_>>()
            .join(",")
    } else {
        String::new()
    };

    // second-highest strictly < onchain
    let mut second_highest: Option<(BigInt, String)> = None;
    if let Some(ref ocw) = onchain_value_wei {
        for (v, src, _bh) in &competitor_values {
            if v < ocw {
                let better = match &second_highest {
                    Some((cur, _)) => v > cur,
                    None => true,
                };
                if better {
                    second_highest = Some((v.clone(), src.clone()));
                }
            }
        }
    }

    // winning-by-value in EXACT-TS set
    let is_winning_bid_highest =
        if let (Some(ref ocw), Some((ref maxv, _))) = (&onchain_value_wei, &max_competitor) {
            ocw >= maxv
        } else {
            true
        };

    // details (from ts-matched rows first, then any exact-hash row in ts-matched)
    let mut block_number: Option<String> =
        ref_rows.iter().find_map(|r| r.row.block_number.clone());
    let mut proposer_pubkey: Option<String> =
        ref_rows.iter().find_map(|r| r.row.proposer_pubkey.clone());
    let mut parent_hash: Option<String> =
        ref_rows.iter().find_map(|r| r.row.parent_hash.clone());
    let mut builder_pubkey: Option<String> =
        ref_rows.iter().find_map(|r| r.row.builder_pubkey.clone());
    let mut proposer_fee_recipient: Option<String> =
        ref_rows.iter().find_map(|r| r.row.proposer_fee_recipient.clone());

    if block_number.is_none() {
        if let Some(rws) = find_row_by_block_hash(onchain_hash.as_deref(), &ref_rows) {
            block_number = rws.row.block_number.clone();
            proposer_pubkey = rws.row.proposer_pubkey.clone().or(proposer_pubkey);
            parent_hash = rws.row.parent_hash.clone().or(parent_hash);
            builder_pubkey = rws.row.builder_pubkey.clone().or(builder_pubkey);
            proposer_fee_recipient =
                rws.row.proposer_fee_recipient.clone().or(proposer_fee_recipient);
        }
    }

    // which proxy delivered
    let onchain_delivered_relay = entry
        .node_id
        .clone()
        .or_else(|| find_delivered_relay_label(onchain_hash.as_deref(), &ref_rows));

    // outcomes
    let forced_proxy_loss_due_equal = is_equal_to_proxy_bid;
    let mut final_is_proxy_win = entry
        .win
        .as_deref()
        .map(|w| w.eq_ignore_ascii_case("yes"))
        .unwrap_or(true);
    if forced_proxy_loss_due_equal {
        final_is_proxy_win = false;
    }

    let (el_inc_wei_opt, el_inc_eth_opt, el_inc_pct_int_opt, el_inc_pct_prec_opt, el_improve_eth) =
        if forced_proxy_loss_due_equal {
            (
                Some("0".to_string()),
                Some("0".to_string()),
                Some(0),
                Some("0.000000000000000000000000000".to_string()),
                Some("0".to_string()),
            )
        } else if final_is_proxy_win {
            if let (Some(ref ocw), Some((ref sh, _))) = (&onchain_value_wei, &second_highest) {
                let diff = ocw - sh;
                let wei_s = diff.to_string();
                let inc_eth = wei_to_eth_str(&diff);
                let (pct_prec, pct_int) = percent_precise(&diff, ocw, 30);
                (
                    Some(wei_s),
                    Some(inc_eth.clone()),
                    Some(pct_int),
                    Some(pct_prec),
                    Some(inc_eth),
                )
            } else {
                let zero = "0".to_string();
                (
                    Some(zero.clone()),
                    Some(zero.clone()),
                    Some(0),
                    Some("0.000000000000000000000000000".to_string()),
                    Some(zero),
                )
            }
        } else {
            // legacy: improvement if not delivered by any endpoint; strict to exact TS set
            let improvement_eth = if !delivered_any {
                if let (Some(maxw), Some(inw)) = (
                    ref_rows
                        .iter()
                        .filter_map(|r| r.row.value.as_deref())
                        .filter_map(parse_bigint)
                        .max_by(bigint_cmp),
                    onchain_value_wei.clone(),
                ) {
                    let diff = &maxw - &inw;
                    if diff.is_negative() || diff.is_zero() {
                        Some("0".to_string())
                    } else {
                        Some(wei_to_eth_str(&diff))
                    }
                } else {
                    None
                }
            } else {
                Some("0".to_string())
            };
            (None, None, None, None, improvement_eth)
        };

    let second_highest_bid_value = second_highest.as_ref().map(|(w, _)| wei_to_eth_str(w));
    let second_higher_bid_delivered_relay = second_highest.as_ref().map(|(_, s)| s.clone());
    let max_endpoint_value_eth = max_competitor.as_ref().map(|(w, _)| wei_to_eth_str(w));

    // fee tiers
    let fee_per_block = {
        let mut fee = dec!(0.0);
        if final_is_proxy_win {
            let uplift_pct = dec_from_opt_str(el_inc_pct_prec_opt.as_deref());
            let uplift_eth = dec_from_opt_str(el_inc_eth_opt.as_deref());
            if uplift_pct > dec!(0) && uplift_eth > dec!(0) {
                if uplift_pct <= dec!(1) {
                    fee = dec!(0.0);
                } else if uplift_pct <= dec!(5) {
                    if uplift_eth >= dec!(0.0015) {
                        fee = dec!(0.0015);
                    }
                } else if uplift_pct <= dec!(9) {
                    if uplift_eth > dec!(0.003) {
                        fee = dec!(0.003);
                    } else if uplift_eth > dec!(0.0015) {
                        fee = dec!(0.0015);
                    }
                } else {
                    if uplift_eth > dec!(0.005) {
                        fee = dec!(0.005);
                    } else if uplift_eth > dec!(0.003) {
                        fee = dec!(0.003);
                    } else if uplift_eth > dec!(0.0015) {
                        fee = dec!(0.0015);
                    }
                }
            }
        }
        fee
    };

    let relay_proxy_win_no_bids_from_relays =
        final_is_proxy_win && second_highest.is_none() && ref_rows.is_empty();

    let rproxy_input_win_marked_lost_due_equal = entry
        .win
        .as_deref()
        .map(|w| w.eq_ignore_ascii_case("yes"))
        .unwrap_or(true)
        && forced_proxy_loss_due_equal;

    let analysis = SlotAnalysis {
        slot,
        onchain_block_hash: onchain_hash,
        matched_by_timestamp,
        delivered_by_any_endpoint: delivered_any,
        delivered_in_ts_matched,
        input_value_eth: onchain_value_eth.clone(),
        max_endpoint_value_eth,
        el_reward_improvement_eth: el_improve_eth,
        endpoints_used,

        is_proxy_win: final_is_proxy_win,
        is_winning_bid_highest,

        el_reward_increase_wei: el_inc_wei_opt,
        el_reward_increase_eth: el_inc_eth_opt,

        onchain_bid_value: onchain_value_eth,
        second_highest_bid_value,
        second_highest_bid_submitted_value: None,
        second_highest_bid_adjusted: false,

        onchain_bid_delivered_relay: onchain_delivered_relay,
        second_higher_bid_delivered_relay,

        el_reward_increase_percentage: el_inc_pct_int_opt,
        el_reward_increase_percent_precise: el_inc_pct_prec_opt,

        equal_to_proxy_bidders,
        is_equal_to_proxy_bid,

        block_number,
        proposer_pubkey,
        parent_hash,
        builder_pubkey,
        proposer_fee_recipient,

        fee_per_block,
        relay_proxy_win_no_bids_from_relays,
        rproxy_input_win_marked_lost_due_equal,
    };

    Ok((analysis, relays_by_ts))
}

// ============== HTTP + helpers ==============

async fn fetch_rows(
    client: &reqwest::Client,
    ep: &Endpoint,
    slot: u64,
) -> Result<Vec<(RelayRow, Value)>> {
    let url_with_slot = if ep.url.contains('?') {
        format!("{}&slot={slot}", ep.url)
    } else {
        format!("{}?slot={slot}", ep.url)
    };
    let mut req = client.get(&url_with_slot);
    if let Some(k) = &ep.api_key {
        req = req.header("X-API-Key", k);
    }
    let mut resp = req
        .send()
        .await
        .with_context(|| format!("GET {}", url_with_slot))?;

    if !resp.status().is_success() {
        let status = resp.status();
        if matches!(
            status,
            StatusCode::BAD_REQUEST | StatusCode::NOT_FOUND | StatusCode::UNAUTHORIZED
        ) {
            let mut req2 = client.get(&ep.url);
            if let Some(k) = &ep.api_key {
                req2 = req2.header("X-API-Key", k);
            }
            sleep(Duration::from_millis(100)).await;
            resp = req2
                .send()
                .await
                .with_context(|| format!("GET {}", ep.url))?;
        }
    }

    if !resp.status().is_success() {
        return Err(anyhow!("HTTP {} from {}", resp.status(), ep.url));
    }

    let text = resp.text().await?;
    let v: Value = serde_json::from_str(&text).with_context(|| "parse relay JSON")?;

    let mut out: Vec<(RelayRow, Value)> = Vec::new();

    if let Some(arr) = v.as_array() {
        for item in arr {
            if item.is_object() {
                if let Ok(row) = serde_json::from_value::<RelayRow>(item.clone()) {
                    out.push((row, item.clone()));
                }
            }
        }
        return Ok(out);
    }

    if let Some(arr) = v.get("data").and_then(|d| d.as_array()) {
        for item in arr {
            if item.is_object() {
                if let Ok(row) = serde_json::from_value::<RelayRow>(item.clone()) {
                    out.push((row, item.clone()));
                }
            }
        }
        return Ok(out);
    }

    if v.is_object() {
        if let Ok(row) = serde_json::from_value::<RelayRow>(v.clone()) {
            out.push((row, v));
        }
        return Ok(out);
    }

    Err(anyhow!("unexpected JSON shape from {}", ep.url))
}

async fn fetch_ultrasound_adjustments(
    client: &reqwest::Client,
    slot: u64,
) -> Result<Vec<UltrasoundAdjustment>> {
    let url = format!(
        "https://relay-analytics.ultrasound.money/ultrasound/v1/data/adjustments?slot={}",
        slot
    );
    let resp = client
        .get(&url)
        .send()
        .await
        .with_context(|| format!("GET {}", url))?;
    if !resp.status().is_success() {
        return Err(anyhow!("HTTP {} from {}", resp.status(), url));
    }
    let text = resp.text().await?;
    let v: Value = serde_json::from_str(&text).with_context(|| "parse ultrasound adjustments JSON")?;

    if let Ok(env) = serde_json::from_value::<UltrasoundAdjustmentEnvelope>(v.clone()) {
        return Ok(env.data);
    }
    if let Ok(single) = serde_json::from_value::<Vec<UltrasoundAdjustment>>(v.clone()) {
        return Ok(single);
    }

    Err(anyhow!("unexpected Ultrasound adjustments JSON shape"))
}

fn onchain_block_hash(entry: &InputEntry) -> Option<String> {
    entry
        .payload_delivered_block_hash
        .as_ref()
        .or(entry.header_delivered_block_hash.as_ref())
        .cloned()
        .map(|s| s.to_lowercase())
}

fn row_slot_eq(r: &RelayRow, slot: u64) -> bool {
    match r.slot.as_deref() {
        Some(s) => s == slot.to_string(),
        None => false,
    }
}

fn block_hash_in_rows(input_hash: Option<&str>, rows: &[RowWithSrc]) -> bool {
    let Some(needle) = input_hash else {
        return false;
    };
    let needle = needle.trim().to_lowercase();
    rows.iter()
        .any(|rs| rs.row.block_hash.as_deref().map(|h| h.trim().eq_ignore_ascii_case(&needle)).unwrap_or(false))
}

fn find_row_by_block_hash(input_hash: Option<&str>, rows: &[RowWithSrc]) -> Option<RowWithSrc> {
    let Some(needle) = input_hash else {
        return None;
    };
    let needle = needle.trim().to_lowercase();
    rows.iter()
        .find(|rs| rs.row.block_hash.as_deref().map(|h| h.trim().eq_ignore_ascii_case(&needle)).unwrap_or(false))
        .cloned()
}

fn find_delivered_relay_label(input_hash: Option<&str>, rows: &[RowWithSrc]) -> Option<String> {
    let Some(needle) = input_hash else {
        return None;
    };
    let needle = needle.trim().to_lowercase();
    for rs in rows {
        if let Some(h) = rs.row.block_hash.as_deref() {
            if h.trim().eq_ignore_ascii_case(&needle) {
                return Some(rs.src.clone());
            }
        }
    }
    None
}

fn is_proxy_node_id_str(s: &str) -> bool {
    s.to_ascii_lowercase().contains("proxy")
}

fn has_proxy_node_id(row: &RowWithSrc) -> bool {
    if let Some(id) = row.raw.get("node_id").and_then(|v| v.as_str()) {
        return is_proxy_node_id_str(id);
    }
    if let Some(ref id) = row.row.node_id {
        return is_proxy_node_id_str(id);
    }
    false
}

fn is_bloxroute_label(label: &str) -> bool {
    let l = label.to_ascii_lowercase();
    l.contains("bloxroute") || l.contains("blxrbdn.com")
}
fn is_ultrasound_label(label: &str) -> bool {
    label.to_ascii_lowercase().contains("ultrasound.money")
}

// ============== JSON reading (array or NDJSON) ==============

fn read_input_entries(path: PathBuf) -> Result<Vec<InputEntry>> {
    // detect array vs NDJSON
    let mut probe = File::open(&path).with_context(|| format!("open {}", path.display()))?;
    let mut first_non_ws: Option<u8> = None;
    let mut b = [0u8; 1];
    loop {
        let n = probe.read(&mut b)?;
        if n == 0 {
            break;
        }
        let c = b[0];
        if !matches!(c, b' ' | b'\t' | b'\n' | b'\r') {
            first_non_ws = Some(c);
            break;
        }
    }

    if first_non_ws == Some(b'[') {
        let f = File::open(&path)?;
        let entries: Vec<InputEntry> =
            serde_json::from_reader(f).with_context(|| "parse input JSON array")?;
        return Ok(entries);
    }

    // NDJSON
    let f = File::open(&path)?;
    let buf = BufReader::new(f);
    let mut out = vec![];
    for (i, line) in buf.lines().enumerate() {
        let l = line?;
        if l.trim().is_empty() {
            continue;
        }
        let v: InputEntry = serde_json::from_str(&l)
            .with_context(|| format!("parse NDJSON line {}", i + 1))?;
        out.push(v);
    }
    Ok(out)
}

fn read_input_entries_by_slot(path: PathBuf) -> Result<HashMap<u64, Vec<InputEntry>>> {
    let entries = read_input_entries(path)?;
    let mut map: HashMap<u64, Vec<InputEntry>> = HashMap::new();
    for e in entries {
        map.entry(e.slot).or_default().push(e);
    }
    Ok(map)
}

// ============== Endpoints + utilities ==============

fn resolve_endpoints_from_env() -> Vec<Endpoint> {
    // env RELAY_ENDPOINTS = "https://...ultrasound.../proposer_header_delivered,https://another"
    let endpoints: Vec<String> = if let Ok(s) = std::env::var("RELAY_ENDPOINTS") {
        s.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect()
    } else {
        vec![
            "https://relay-analytics.ultrasound.money/relay/v1/data/bidtraces/proposer_header_delivered"
                .to_string(),
        ]
    };

    // env RELAY_API_KEYS aligns 1:1 or is a single key for all
    let mut keys: Vec<String> = if let Ok(s) = std::env::var("RELAY_API_KEYS") {
        s.split(',')
            .map(|x| x.trim().to_string())
            .filter(|x| !x.is_empty())
            .collect()
    } else {
        // demo/default (optional)
        vec!["fcd8ed67ecf8d0ec3bece1da340c899c84104e71abcbeee702013fbfebe95416".to_string()]
    };

    if keys.is_empty() {
        return endpoints
            .into_iter()
            .map(|u| Endpoint { url: u, api_key: None })
            .collect();
    } else if keys.len() == 1 {
        let k = keys.remove(0);
        return endpoints
            .into_iter()
            .map(|u| Endpoint {
                url: u,
                api_key: Some(k.clone()),
            })
            .collect();
    } else if keys.len() >= endpoints.len() {
        return endpoints
            .into_iter()
            .zip(keys.into_iter())
            .map(|(u, k)| Endpoint {
                url: u,
                api_key: Some(k),
            })
            .collect();
    } else {
        // fewer keys than endpoints -> reuse first key
        let k0 = keys.remove(0);
        return endpoints
            .into_iter()
            .map(|u| Endpoint {
                url: u,
                api_key: Some(k0.clone()),
            })
            .collect();
    }
}

// ============== Math/format helpers ==============

fn local_stamp() -> (String, String) {
    let now = Local::now();
    (
        now.format("%Y-%m-%d").to_string(),
        now.format("%Hh_%Mm_%Ss").to_string(),
    )
}

fn parse_bigint(s: &str) -> Option<BigInt> {
    s.parse::<BigInt>().ok()
}
fn bigint_cmp(a: &BigInt, b: &BigInt) -> Ordering {
    a.cmp(b)
}

fn wei_to_eth_str(wei: &BigInt) -> String {
    let neg = wei.is_negative();
    let s = wei.abs().to_string();
    if s.len() <= 18 {
        let mut z = "0.".to_string();
        z.push_str(&"0".repeat(18 - s.len()));
        z.push_str(&s);
        if neg {
            format!("-{}", z)
        } else {
            z
        }
    } else {
        let split = s.len() - 18;
        let (intp, frac) = s.split_at(split);
        let mut out = String::new();
        out.push_str(intp);
        out.push('.');
        out.push_str(frac);
        if neg {
            format!("-{}", out)
        } else {
            out
        }
    }
}

fn eth_str_to_wei(eth: &str) -> BigInt {
    let trimmed = eth.trim();
    let neg = trimmed.starts_with('-');
    let s = if neg { &trimmed[1..] } else { trimmed };
    let parts: Vec<&str> = s.split('.').collect();
    let (int_part, frac_part) = match parts.len() {
        1 => (parts[0], ""),
        _ => (parts[0], parts[1]),
    };
    let mut frac = frac_part.to_string();
    if frac.len() > 18 {
        frac.truncate(18);
    } else {
        frac.push_str(&"0".repeat(18 - frac.len()));
    }
    let mut full = String::new();
    full.push_str(if int_part.is_empty() { "0" } else { int_part });
    full.push_str(&frac);
    let full = full.trim_start_matches('0');
    let wei = if full.is_empty() {
        BigInt::zero()
    } else {
        full.parse::<BigInt>()
            .unwrap_or_else(|_| BigInt::zero())
    };
    if neg { -wei } else { wei }
}

// Return (precise string with `scale` decimals, integer floor)
fn percent_precise(numer: &BigInt, denom: &BigInt, scale: usize) -> (String, i64) {
    if denom.is_zero() || numer.is_zero() {
        return ("0.".to_string() + &"0".repeat(scale), 0);
    }
    let ten = BigInt::from(10u32);
    let factor = ten.pow(scale as u32);
    let scaled = numer * &BigInt::from(100u32) * &factor;
    let q = scaled / denom;
    let mut s = q.to_string();
    if s.len() <= scale {
        let zeros = "0".repeat(scale - s.len() + 1);
        s = format!("{}{}", zeros, s);
    }
    let split = s.len() - scale;
    let out = format!("{}.{:}", &s[..split], &s[split..]);
    let int_part = &s[..split];
    let int_floor = int_part.parse::<i64>().unwrap_or(0);
    (out, int_floor)
}

fn dec_from_opt_str(s: Option<&str>) -> Decimal {
    s.and_then(|t| Decimal::from_str(t).ok()).unwrap_or(Decimal::ZERO)
}
fn fmt18(d: Decimal) -> String {
    format!("{:.18}", d)
}
fn fmt18_truncate3(d: Decimal) -> String {
    let s = format!("{:.18}", d); // always 18 decimal places
    if let Some(dot) = s.find('.') {
        let int_part = &s[..dot];
        let frac_part = &s[dot + 1..];

        if frac_part.len() > 3 {
            // drop last 3 digits
            let truncated = &frac_part[..frac_part.len() - 3];
            format!("{}.{}", int_part, truncated)
        } else {
            // if somehow shorter, just return as-is
            s
        }
    } else {
        s
    }
}
fn truncate3_decimal(d: Decimal) -> Decimal {
    // scale is always 18 decimals for ETH values
    let s = format!("{:.18}", d);
    if let Some(dot) = s.find('.') {
        let int_part = &s[..dot];
        let frac_part = &s[dot + 1..];

        if frac_part.len() > 3 {
            let truncated = &frac_part[..frac_part.len() - 3];
            let new_str = format!("{}.{}", int_part, truncated);
            Decimal::from_str(&new_str).unwrap_or(d)
        } else {
            d
        }
    } else {
        d
    }
}
