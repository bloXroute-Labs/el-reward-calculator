#![allow(unused_variables)]
mod log_source;
use log_source::mevboost_json;
use log_source::mevboost_text;
use log_source::commitboost_text;
use log_source::commitboost_json;
use log_source::vouch;
use log_source::stats_writer;
use crate::log_source::common::filter_valid_slot_infos;
use crate::log_source::types::{CommitBoostSlotInfo,SlotInfo,Bid};
use chrono::Utc;
use std::collections::HashMap;
use std::fs;
use std::fs::File;
use std::io::BufRead;
use std::io::BufReader;
use std::io::{self, ErrorKind};
use  std::io::Result as IoResult;
use std::str::FromStr;
use std::env;
use env_logger;

//time="2024-10-09T09:17:12.404Z" level=info msg="submitBlindedBlock request start - 1404 milliseconds into slot 10136784"
// blockHash=0xae2c0d7e87e7eaeae842143db2970243e78a965f8390a4ea0f59de0b5403e78b
//     genesisTime=1606824023 method=getPayload msIntoSlot=1404
//     parentHash=0x8cff1dbd053fbc22fd3376fd863f644d8fe9ff4f32802ba090d0be1708ed1fda
//     slot=10136784 slotTimeSec=12 slotUID=9fc9db4c-46a1-493f-b9cd-7bc450c81e18
//     ua=Lighthouse/v5.3.0-d6ba8c3 version=1.8
//
// time="2024-09-27T17:15:00.988Z" level=info msg="received payload from relay"
// blockHash=0x012d8bb7b700313060ca620f96ba69a3fed405ddd9a959b6a4bb1e038bb94f89
// method=getPayload parentHash=0x70a835e90e4cae6c513d422f075eb22be7be0765d63b968340ff11f8d89b012b slot=10052773 slotUID=2fd9298d-f6fe-4a29-b303-57ebda0bee6b ua=Lighthouse/v5.3.0-d6ba8c3
// url="https://bloxroute.regulated.blxrbdn.com/eth/v1/builder/blinded_blocks" version=1.8




#[derive(Debug)]
enum LogSource {
    MevboostJson,
    MevboostText,
    CommitboostJson,
    CommitboostText,
    Vouch,
}

impl FromStr for LogSource {
    type Err = io::Error;
    fn from_str(input: &str) -> std::result::Result<Self, Self::Err> {
        match input.to_lowercase().as_str() {
            "mevboost_json" => Ok(LogSource::MevboostJson),
            "mevboost_text" => Ok(LogSource::MevboostText),
            "commitboost_json" => Ok(LogSource::CommitboostJson),
            "commitboost_text" => Ok(LogSource::CommitboostText),
            "vouch" => Ok(LogSource::Vouch),
            _ => Err(io::Error::new(
                ErrorKind::Other,
                format!("Invalid LogSource '{}'. Must be one of mevboost_json, mevboost_text, commitboost_json, commitboost_text, vouch.", input),
            )),
        }
    }
}


pub type SlotInfos = HashMap<String, HashMap<String, SlotInfo>>; // slot -> (slot_uid -> SlotInfo)
pub type CommitBoostSlotInfos = HashMap<String, HashMap<String, CommitBoostSlotInfo>>;

pub type FinalSlotInfos = HashMap<String, SlotInfo>;
pub type FinalSlotInfosCommitBoost = HashMap<String, CommitBoostSlotInfo>;
// // for SlotInfo
// let selected_slot_infos = select_final_slot_infos_generic::<SlotInfo>(&mevboost_slot_infos);

// // for CommitBoostSlotInfo
// let selected_commit_infos = select_final_slot_infos_generic::<CommitBoostSlotInfo>(&commitboost_slot_infos);

fn main() -> IoResult<()>  {
    if env::var("RUST_LOG").is_err() {
           env::set_var("RUST_LOG", "info");
       }
       env_logger::init();

       let args: Vec<String> = env::args().collect();
       if args.len() < 3 {
           eprint!("either filename or validator client id is missing");
           std::process::exit(1);
       }
       let filename = &args[1];
       let validator_client_id_flag = &args[2];
       let output_format = args.get(3).map(|s| s.as_str()).unwrap_or("json");

       let log_source = match LogSource::from_str(validator_client_id_flag) {
           Ok(source) => source,
           Err(err) => {
               eprintln!("{}", err);
               std::process::exit(1);
           }
       };
       let file = File::open(filename)?;
       let reader = BufReader::new(file);

       let now = Utc::now();
       let date_str = Utc::now().format("%d_%m_%Y").to_string();
       let time_str = now.format("%H_%M_%S").to_string();
       let folder_path = format!("slot_infos/{}/", date_str);
       fs::create_dir_all(&folder_path)?;

       match log_source {
           LogSource::CommitboostJson => {
               let mut slot_infos: CommitBoostSlotInfos = HashMap::new();
               commitboost_json::parse_file_content(reader, &mut slot_infos);
               // Always select final infos once
               let selected_infos = stats_writer::select_final_slot_infos_generic(&slot_infos);

               // after finalize_slot_infos(...)
               let (all_infos, selected_infos, selected_infos_map, skipped) = filter_valid_slot_infos(&slot_infos, "commit_boost_json");

               match output_format {
                   "csv" => {
                       stats_writer::write_csv_generic(&selected_infos_map, &folder_path, &date_str, &time_str)?;
                   }
                   _ => {
                       stats_writer::write_json_generic(&selected_infos_map, &folder_path, &date_str, &time_str)?;
                   }
               }

               //stats_writer::write_summary_generic(&selected_infos_map, &folder_path, &date_str, &time_str)?;
               stats_writer::write_summary_generic(
                   &selected_infos_map,
                   &folder_path,
                   &date_str,
                   &time_str,
                   &selected_infos, // full vec passed for debug
                   &skipped,
               )?;

           }
           LogSource::CommitboostText => {
               let mut slot_infos: CommitBoostSlotInfos = HashMap::new();
               for line in reader.lines() {
                   match line {
                       Ok(line) => commitboost_text::process_lines(line, &mut slot_infos),
                       Err(e) => eprintln!("failed to read lines: {}", e),
                   }
               }
               // Always select final infos once
               let selected_infos = stats_writer::select_final_slot_infos_generic(&slot_infos);

               // after finalize_slot_infos(...)
               let (all_infos, selected_infos, selected_infos_map, skipped) = filter_valid_slot_infos(&slot_infos, "commit_boost_text");

               match output_format {
                   "csv" => {
                       stats_writer::write_csv_generic(&selected_infos_map, &folder_path, &date_str, &time_str)?;
                   }
                   _ => {
                       stats_writer::write_json_generic(&selected_infos_map, &folder_path, &date_str, &time_str)?;
                   }
               }

               //stats_writer::write_summary_generic(&selected_infos_map, &folder_path, &date_str, &time_str)?;
               stats_writer::write_summary_generic(
                   &selected_infos_map,
                   &folder_path,
                   &date_str,
                   &time_str,
                   &selected_infos, // full vec passed for debug
                   &skipped,
               )?;

           }

           LogSource::MevboostJson => {
               let mut slot_infos: SlotInfos = HashMap::new();
               mevboost_json::parse_file_content(reader, &mut slot_infos);
               // Always select final infos once
               let selected_infos = stats_writer::select_final_slot_infos_generic(&slot_infos);

               // after finalize_slot_infos(...)
               let (all_infos, selected_infos, selected_infos_map, skipped) = filter_valid_slot_infos(&slot_infos,"mev_boost_json");

               match output_format {
                   "csv" => {
                       stats_writer::write_csv_generic(&selected_infos_map, &folder_path, &date_str, &time_str)?;
                   }
                   _ => {
                       stats_writer::write_json_generic(&selected_infos_map, &folder_path, &date_str, &time_str)?;
                   }
               }

               //stats_writer::write_summary_generic(&selected_infos_map, &folder_path, &date_str, &time_str)?;
               stats_writer::write_summary_generic(
                   &selected_infos_map,
                   &folder_path,
                   &date_str,
                   &time_str,
                   &selected_infos, // full vec passed for debug
                   &skipped,
               )?;
           }

           LogSource::Vouch => {
               let mut slot_infos: SlotInfos = HashMap::new();
               vouch::parse_file_content(reader, &mut slot_infos);
               // Always select final infos once
               let selected_infos = stats_writer::select_final_slot_infos_generic(&slot_infos);
               // after finalize_slot_infos(...)
               let (all_infos_map, selected_infos, selected_infos_map, skipped) = filter_valid_slot_infos(&slot_infos,"vouch");

               match output_format {
                   "csv" => {
                       stats_writer::write_csv_generic(&selected_infos_map, &folder_path, &date_str, &time_str)?;
                   }
                   _ => {
                       stats_writer::write_json_generic(&all_infos_map, &folder_path, &date_str, &time_str)?;
                   }
               }

               //stats_writer::write_summary_generic(&selected_infos_map, &folder_path, &date_str, &time_str)?;
               stats_writer::write_summary_generic(
                   &selected_infos_map,
                   &folder_path,
                   &date_str,
                   &time_str,
                   &selected_infos, // full vec passed for debug
                   &skipped,
               )?;
           }

           LogSource::MevboostText => {
               let mut slot_infos: SlotInfos = HashMap::new();

               for line in reader.lines() {
                   match line {
                       Ok(line) => mevboost_text::process_lines_first_pass(line, &mut slot_infos),
                       Err(e) => eprintln!("failed to read lines: {}", e),
                   }
               }

               mevboost_text::finalize_slot_infos(&mut slot_infos);
               // after finalize_slot_infos(...)
               let (all_infos, selected_infos, selected_infos_map, skipped) = filter_valid_slot_infos(&slot_infos,"mev_boost_text");

               match output_format {
                   "csv" => {
                       stats_writer::write_csv_generic(&selected_infos_map, &folder_path, &date_str, &time_str)?;
                   }
                   _ => {
                       stats_writer::write_json_generic(&selected_infos_map, &folder_path, &date_str, &time_str)?;
                   }
               }

               //stats_writer::write_summary_generic(&selected_infos_map, &folder_path, &date_str, &time_str)?;
               stats_writer::write_summary_generic(
                   &selected_infos_map,
                   &folder_path,
                   &date_str,
                   &time_str,
                   &selected_infos, // full vec passed for debug
                   &skipped,
               )?;

           }

       }

       Ok(())
}
