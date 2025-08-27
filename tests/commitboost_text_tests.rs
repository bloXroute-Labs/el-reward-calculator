// tests/commitboost_text_tests.rs

use el_reward_calculator::CommitBoostSlotInfos;
use el_reward_calculator::log_source::commitboost_text::{process_lines, post_process_all_slots};
use el_reward_calculator::log_source::types::{CommitBoostSlotInfo, SlotTrait};
use rust_decimal::Decimal;
use rust_decimal_macros::dec;
use rust_decimal::prelude::ToPrimitive;

// ---------- Helpers ----------

fn slot_uid(slot: &str, parent_hash: &str) -> String {
    format!("{}_{}", slot, parent_hash)
}

fn feed(line: &str, infos: &mut CommitBoostSlotInfos) {
    process_lines(line, infos);
}

fn info_ref<'a>(infos: &'a CommitBoostSlotInfos, slot: &str, parent_hash: &str) -> &'a CommitBoostSlotInfo {
    let m = infos.get(slot).expect("slot map present");
    let uid = slot_uid(slot, parent_hash);
    m.get(&uid).expect("slot_uid present")
}

//  constants

const SLOT: &str = "11955232";
const PARENT: &str = "0x86ae50ba15ba533f4fbc080594b5d2aaa80a6539d9ac2fc5d0e8a283f047d554";

const TS1: &str = "2025-06-18T22:46:47.249303Z";
const TS2: &str = "2025-06-18T22:46:47.304159Z";
const TS3: &str = "2025-06-18T22:46:48.076543Z";
const TS4: &str = "2025-06-18T22:46:48.078150Z";
const TS_SUM: &str = "2025-06-18T22:46:48.092420Z";
const TS_SUBMIT: &str = "2025-06-18T22:46:49.000000Z";

const REQ1: &str = "54576fcf-dcbd-4fc6-8db9-2aaee1e0879e";
const REQ2: &str = "a2265eea-db13-4e8b-a791-6768205efc08";

const BHASH_NONPROXY: &str = "0x676f2706f97411ccd39c4b52c6c0c5cd1689d964cb1b677e539674809dce0074";
const BHASH_PROXY: &str    = "0x5a1ded8ba01ab2f8057bd7ab0702433d42b890e38050d85dbf69644bf98c0f26";

const NONPROXY_A: &str = "renzo_primev_bloxroute_regulated";
const NONPROXY_B: &str = "renzo_primev_bloxroute_maxprofit";
const PROXY_1: &str    = "renzo_bloxroute_proxy1";
const PROXY_2: &str    = "renzo_bloxroute_proxy2";

const V_NONPROXY: Decimal = dec!(0.016659245102584224);
const V_PROXY: Decimal    = dec!(0.018038197956769141);

fn sys_prefix(ts: &str) -> String {
    format!("Jun 18 22:46:48 commit-boost-pbs[1234]: {ts} ")
}

fn new_header_line(ts: &str, relay: &str, value: Decimal, block_hash: &str, req_id: &str) -> String {
    format!(
        "{}DEBUG : received new header relay_id=\"{}\" latency=100ms version=\"electra\" value_eth=\"{}\" block_hash={} method=/eth/v1/builder/header/{{slot}}/{{parent_hash}}/{{pubkey}} req_id={} slot={} parent_hash={} validator=0xabc...",
        sys_prefix(ts), relay, value, block_hash, req_id, SLOT, PARENT
    )
}

// “Summary” header (no relay_id) — must NOT create a bid.
fn summary_header_line(ts: &str, value: Decimal, block_hash: &str, req_id: &str) -> String {
    format!(
        "{}INFO  : received header value_eth=\"{}\" block_hash={} method=/eth/v1/builder/header/{{slot}}/{{parent_hash}}/{{pubkey}} req_id={} slot={} parent_hash={} validator=0xabc...",
        sys_prefix(ts), value, block_hash, req_id, SLOT, PARENT
    )
}

fn unblinded_block_line(ts: &str, block_hash: &str, block_number: u64, req_id: &str) -> String {
    format!(
        "{}INFO  : received unblinded block method=/eth/v1/builder/blinded_blocks req_id={} slot={} block_hash={} block_number={} parent_hash={}",
        sys_prefix(ts), req_id, SLOT, block_hash, block_number, PARENT
    )
}

// ---------- Tests ----------

#[test]
fn ignores_lines_without_rfc3339_and_parses_quoted_kv() {
    let mut infos: CommitBoostSlotInfos = Default::default();

    let bad = "Jun 18 22:46:48 commit-boost-pbs[1]: DEBUG : received new header relay_id=\"x\"";
    feed(bad, &mut infos);
    assert!(infos.is_empty(), "lines without RFC3339 timestamp must be ignored");

    let line = format!(
        "{}DEBUG : received new header relay_id=\"renzo primev\" value_eth=\"0.1\" block_hash=0xabc method=/eth/v1/builder/header/{{slot}}/{{parent_hash}}/{{pubkey}} req_id=R slot={} parent_hash={}",
        sys_prefix(TS1), SLOT, PARENT
    );
    feed(&line, &mut infos);

    let _ = info_ref(&infos, SLOT, PARENT); // exists
}

#[test]
fn summary_header_does_not_create_bids() {
    let mut infos: CommitBoostSlotInfos = Default::default();

    // Two non-proxy bids
    feed(&new_header_line(TS1, NONPROXY_A, V_NONPROXY, BHASH_NONPROXY, REQ1), &mut infos);
    feed(&new_header_line(TS2, NONPROXY_B, V_NONPROXY, BHASH_NONPROXY, REQ1), &mut infos);

    // Two proxy bids at higher value
    feed(&new_header_line(TS3, PROXY_2, V_PROXY, BHASH_PROXY, REQ1), &mut infos);
    feed(&new_header_line(TS4, PROXY_1, V_PROXY, BHASH_PROXY, REQ1), &mut infos);

    // Summary header (no relay_id). If mishandled as a bid, it could create a fake competitor.
    feed(&summary_header_line(TS_SUM, V_PROXY, BHASH_PROXY, REQ1), &mut infos);

    // Submit unblinded block for the proxy hash to lock selection
    feed(&unblinded_block_line(TS_SUBMIT, BHASH_PROXY, 22734443, REQ1), &mut infos);

    post_process_all_slots(&mut infos);
    let info = info_ref(&infos, SLOT, PARENT);

    assert!(info.is_winning_bid_highest());
    assert!(info.is_proxy_win());
    assert_eq!(info.get_onchain_bid_value(), V_PROXY);
    assert_eq!(info.get_block_hash(), BHASH_PROXY);
    // Should pick proxy1 (lexicographically smaller than proxy2) as the delivered relay
    assert_eq!(info.get_onchain_bid_delivered_relay(), PROXY_1);
}

#[test]
fn unblinded_block_selects_matching_req_and_sets_block_number() {
    let mut infos: CommitBoostSlotInfos = Default::default();

    // Non-proxy bids (REQ1)
    feed(&new_header_line(TS1, NONPROXY_A, V_NONPROXY, BHASH_NONPROXY, REQ1), &mut infos);
    feed(&new_header_line(TS2, NONPROXY_B, V_NONPROXY, BHASH_NONPROXY, REQ1), &mut infos);

    // Proxy bids (REQ2)
    feed(&new_header_line(TS3, PROXY_2, V_PROXY, BHASH_PROXY, REQ2), &mut infos);
    feed(&new_header_line(TS4, PROXY_1, V_PROXY, BHASH_PROXY, REQ2), &mut infos);

    // Submit unblinded block for the proxy hash
    feed(&unblinded_block_line(TS_SUBMIT, BHASH_PROXY, 22734443, REQ2), &mut infos);

    let info = info_ref(&infos, SLOT, PARENT);
    assert_eq!(info.get_block_hash(), BHASH_PROXY);
    assert_eq!(info.get_block_number(), "22734443");
}

#[test]
fn post_process_detects_proxy_win_and_computes_uplift_and_pct() {
    let mut infos: CommitBoostSlotInfos = Default::default();

    // Two non-proxy and two proxy bids
    feed(&new_header_line(TS1, NONPROXY_A, V_NONPROXY, BHASH_NONPROXY, REQ1), &mut infos);
    feed(&new_header_line(TS2, NONPROXY_B, V_NONPROXY, BHASH_NONPROXY, REQ1), &mut infos);
    feed(&new_header_line(TS3, PROXY_2,    V_PROXY,    BHASH_PROXY,    REQ1), &mut infos);
    feed(&new_header_line(TS4, PROXY_1,    V_PROXY,    BHASH_PROXY,    REQ1), &mut infos);

    // Unblinded block locks the proxy hash
    feed(&unblinded_block_line(TS_SUBMIT, BHASH_PROXY, 22734443, REQ1), &mut infos);

    post_process_all_slots(&mut infos);
    let info = info_ref(&infos, SLOT, PARENT);

    // Top value = proxy
    assert!(info.is_winning_bid_highest());
    assert!(info.is_proxy_win());
    assert_eq!(info.get_onchain_bid_value(), V_PROXY);
    assert_eq!(info.get_block_hash(), BHASH_PROXY);

    // Uplift (EL) = top - best_nonproxy
    let expected_uplift = V_PROXY - V_NONPROXY; // 0.0013789528541849165
    assert_eq!(info.get_el_reward_eth(), expected_uplift);

    // Precise percent = uplift / top * 100
    let expected_pct = (expected_uplift / V_PROXY) * dec!(100);
    assert_eq!(info.get_el_reward_precise(), expected_pct);
    assert_eq!(info.get_el_reward_percentage(), expected_pct.round().to_u64().unwrap());

    // Delivered relay picked deterministically (proxy1 < proxy2 by host string)
    assert_eq!(info.get_onchain_bid_delivered_relay(), PROXY_1);

    // Second highest (best non-proxy)
    assert_eq!(info.get_second_highest_bid_value(), V_NONPROXY);
}

#[test]
fn post_process_marks_loss_on_tie_with_nonproxy_at_top() {
    let mut infos: CommitBoostSlotInfos = Default::default();

    // Create a top-value tie between a proxy and a non-proxy (different hashes)
    let tie_val = dec!(0.02);

    feed(&new_header_line(TS1, PROXY_1,    tie_val, "0xaaa", REQ1), &mut infos);
    feed(&new_header_line(TS2, NONPROXY_A, tie_val, "0xbbb", REQ1), &mut infos);

    post_process_all_slots(&mut infos);
    let info = info_ref(&infos, SLOT, PARENT);

    assert!(!info.is_proxy_win(), "tie with non-proxy at top should be LOSS");
    assert!(info.is_equal_to_proxy_bid(), "flag tie with non-proxy");

    // On loss, EL and fee must be zero
    assert_eq!(info.get_el_reward_eth(), Decimal::ZERO);
    assert_eq!(info.get_fee_per_block(), dec!(0.0));

    // equal_to_proxy_bidders string should include the non-proxy host
    let bidders = info.get_equal_to_proxy_bidders().to_string();
    assert!(bidders.contains(NONPROXY_A));
}
