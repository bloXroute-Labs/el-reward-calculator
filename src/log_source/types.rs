use serde::Deserialize;
use serde::Serialize;
use serde::Serializer;
use ethers::types::U256;
use std::i64;

#[derive(Debug,Default,Serialize, Deserialize)]
#[allow(dead_code)]
pub struct LogEntryVouch {
    pub level: String,
    pub service: String,
    #[serde(rename = "impl")]
    pub impl_field: String,
    pub slot: i64,
    pub provider: String,
    pub value: String,
    #[serde(rename = "value_delta")]
    pub value_delta: String,
    pub score: String,
    #[serde(rename = "score_delta")]
    pub score_delta: String,
    pub selected: bool,
    pub time: String,
    pub message: String,
}
#[derive(Default, Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MEVBoostJSONLogEntry {
    pub level: String,
    pub method: String,
    pub msg: String,
    pub slot: String,
    #[serde(rename = "slotUID", default)]
    pub slot_uid: String,
    pub time: String,
    #[serde(default)]
    pub block_hash: String,
    #[serde(default)]
    pub parent_hash: String,
    #[serde(default)]
    pub ua: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub version: String,
    // Optional only needed for `getHeader` -> bid received
    #[serde(default)]
    pub block_number: Option<i64>,
    #[serde(default)]
    pub pubkey: Option<String>,
    #[serde(default)]
    pub tx_root: Option<String>,
    #[serde(default)]
    pub value: Option<String>,
}

#[derive(Debug,Default,Serialize, Deserialize)]
#[allow(dead_code)]
pub struct LogEntry {
    pub client: String,
    pub filename: String,
    pub hostname: String,
    pub log_format_version: String,
    pub logstamp: u64,
    pub message: Message,
    pub network: String,
    pub syslog_identifier: String,
}

#[derive(Debug,Default,Serialize, Deserialize)]
#[allow(non_snake_case, dead_code)]
pub struct Message {
    pub blockHash: String,
    pub level: String,
    pub method: String,
    pub msg: String,
    pub parentHash: String,
    pub slot: String,
    pub slotUID: String,
    pub time: String,
    pub ua: String,
    pub url: Option<String>,
    pub version: String,
    pub blockNumber: Option<u64>,
    pub pubkey: Option<String>,
    pub txRoot: Option<String>,
    pub value: Option<String>,
}

#[derive(Debug,Default,Serialize, Deserialize)]
pub struct SlotInfo {
    pub slot_uid: String,
    pub slot: String,
    pub block_number: String,
    pub info: RequestInfo,
    pub is_proxy_win: bool,
    pub is_winning_bid_highest: bool,
    #[serde(serialize_with = "u256_to_string")]
    pub el_reward_increase_wei: U256,
    #[serde(serialize_with = "float_to_fixed")]
    pub el_reward_increase_eth: f64,
    pub onchain_bid_value: f64,
    pub second_highest_bid_value: f64,
    pub onchain_bid_delivered_relay: String,
    pub second_higher_bid_delivered_relay: String,
    pub is_payload_received: bool,
    pub el_reward_increase_percentage: u64,
    pub el_reward_increase_percent_precise: f64,
    pub equal_to_proxy_bidders: String,
    pub is_equal_to_proxy_bid: bool,
    pub fee_per_block: f64,
}

pub fn u256_to_string<S>(value: &U256, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&value.to_string())
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, PartialOrd)]
pub struct RequestInfo {
    pub header_start_ms_into_slot: i64,
    pub bids: Vec<Bid>,
    pub payload_start_ms_into_slot: i64,
    pub block_hash: String,
}

#[derive(Debug, Default, Serialize, Deserialize, PartialEq, PartialOrd, Clone)]
pub struct Bid {
    pub timestamp: i64,
    pub pubkey: String,
    pub block_hash: String,
    pub parent_hash: String,
    pub block_number: String,
    pub slot: String,
    pub ua: String,
    pub relay: String,
    pub bid_value: f64,
}


#[derive(Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub struct SlotInfoWithoutBids<'a> {
    pub slot_uid: &'a str,
    pub slot: &'a str,
    pub block_number: &'a str,
    pub header_start_ms_into_slot: i64,
    pub payload_start_ms_into_slot: i64,
    pub block_hash: &'a str,
    pub is_proxy_win: bool,
    pub is_winning_bid_highest: bool,
    #[serde(serialize_with = "float_to_fixed")]
    pub el_reward_increase_eth: f64,
    pub el_reward_increase_wei: U256,
    #[serde(serialize_with = "float_to_fixed")]
    pub onchain_bid_value: f64,
    pub onchain_bid_delivered_relay: String,
    #[serde(serialize_with = "float_to_fixed")]
    pub second_highest_bid_value: f64,
    pub second_higher_bid_delivered_relay: String,
    pub is_payload_received: bool,
    pub el_reward_increase_percentage: u64,
    #[serde(serialize_with = "float_to_fixed")]
    pub el_reward_increase_percent_precise: f64,
    pub equal_to_proxy_bidders: String,
    pub is_equal_to_proxy_bid: bool,
    #[serde(serialize_with = "float_to_fixed")]
    pub fee_per_block: f64,
}

pub fn float_to_fixed<S>(x: &f64, serializer: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    serializer.serialize_str(&format!("{:.18}", x))
}

impl SlotInfo {
    pub fn new(slot_uid: String) -> Self {
        Self {
            slot_uid,
            slot: String::new(),
            block_number: String::new(),
            info: Default::default(),
            is_proxy_win: false,
            el_reward_increase_wei: U256::default(),
            el_reward_increase_eth: 0.0,
            onchain_bid_value: 0.0,
            second_highest_bid_value: 0.0,
            onchain_bid_delivered_relay: String::new(),
            second_higher_bid_delivered_relay: String::new(),
            is_winning_bid_highest: false,
            is_payload_received: false,
            el_reward_increase_percentage: 0,
            el_reward_increase_percent_precise: 0.0,
            equal_to_proxy_bidders:String::new(),
            is_equal_to_proxy_bid: false,
            fee_per_block:0.0,
        }
    }

    // initializes with both slot_uid and slot
    #[allow(dead_code)]
    pub fn new_with_slot_uid_and_slot(slot_uid: String, slot: String) -> Self {
        Self {
            slot_uid,
            slot,
            block_number: String::new(),
            info: Default::default(),
            is_proxy_win: false,
            el_reward_increase_wei: U256::default(),
            el_reward_increase_eth: 0.0,
            onchain_bid_value: 0.0,
            second_highest_bid_value: 0.0,
            onchain_bid_delivered_relay: String::new(),
            second_higher_bid_delivered_relay: String::new(),
            is_winning_bid_highest: false,
            is_payload_received: false,
            el_reward_increase_percentage: 0,
            el_reward_increase_percent_precise: 0.0,
            equal_to_proxy_bidders:String::new(),
            is_equal_to_proxy_bid: false,
            fee_per_block:0.0,
        }
    }

}
