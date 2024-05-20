use bitcoin::ScriptBuf;
use bitcoin::Address;

use crate::constants::NETWORK;

#[derive(PartialEq)]
pub enum ScriptType {
  P2PK,
  P2PKH,
  P2SH,
  P2WPKH,
  P2WSH,
  P2TR,
  Empty,
  OpReturn,
  Unknown,
}

impl From<&ScriptBuf> for ScriptType {
  fn from(script: &ScriptBuf) -> Self {
      if script.is_p2pk() {
          return ScriptType::P2PK;
      }

      if script.is_p2pkh() {
          return ScriptType::P2PKH;
      }

      if script.is_p2sh() {
          return ScriptType::P2SH;
      }

      if script.is_p2wpkh() {
          return ScriptType::P2WPKH;
      }

      if script.is_p2wsh() {
          return ScriptType::P2WSH;
      }

      if script.is_p2tr() {
          return ScriptType::P2TR;
      }

      if script.is_empty() {
          return ScriptType::Empty;
      }

      if script.is_op_return() {
          return ScriptType::OpReturn;
      }

      ScriptType::Unknown
  }
}

impl ToString for ScriptType {
  fn to_string(&self) -> String {
      match self {
          ScriptType::P2PK => "P2PK".to_string(),
          ScriptType::P2PKH => "P2PKH".to_string(),
          ScriptType::P2SH => "P2SH".to_string(),
          ScriptType::P2WPKH => "P2WPKH".to_string(),
          ScriptType::P2WSH => "P2WSH".to_string(),
          ScriptType::P2TR => "P2TR".to_string(),
          ScriptType::Empty => "Empty".to_string(),
          ScriptType::OpReturn => "OpReturn".to_string(),
          ScriptType::Unknown => "UNKNOWN".to_string(),
      }
  }
}

pub fn pretty_print_script(script: &ScriptBuf) -> String {
  let script_type = ScriptType::from(script);

  match script_type {
      ScriptType::P2PK => {
          format!("{:<10} {:<40}", script_type.to_string(), match script.p2pk_public_key() {
              Some(pubkey) => pubkey.to_string(),
              None => "Failed to parse P2PK pubkey".to_string(),
          })
      }
      ScriptType::Unknown | ScriptType::OpReturn | ScriptType::Empty => {
          format!("{:<50} {}", script_type.to_string(), script.to_string())
      }
      _ => {
          format!(
              "{:<10} {:<40}",
              script_type.to_string(),
              match Address::from_script(script, NETWORK) {
                  Ok(address) => address.to_string(),
                  Err(_) => "Failed to parse script address".to_string(),
              }
          )
      }
  }
}
