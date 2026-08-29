// SPDX-License-Identifier: MPL-2.0
//! Deterministic binary serialization for committed bytecode bundles.
//!
//! The on-disk format is a small versioned envelope around a Postcard payload.
//! Source strings and nested codes are interned so shared [`Arc`] values are
//! not repeatedly stored. Decoding never executes bytecode and verifies every
//! reconstructed code before returning it.

use crate::bytecode::{BytecodeVersion, Chunk, Constant, FinallyHandler, Function, UpvalueSpec};
use crate::typecheck::{BuiltinType, LiteralType, Type};
use crate::value::SymbolId;
use num_bigint::{BigInt, Sign};
use oxc_span::Span;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;

/// Current version of the bytecode bundle container format.
pub const BYTECODE_BUNDLE_VERSION: u16 = 4;

const MAGIC: &[u8; 8] = b"BLITZBC\0";
const HEADER_LEN: usize = MAGIC.len() + 2 + 8 + 4;
const MAX_BUNDLE_BYTES: usize = 64 * 1024 * 1024;
const MAX_CHUNK_DEPTH: usize = 256;

/// A named top-level code in a precompiled bundle.
#[derive(Clone, Debug)]
pub struct NamedChunk {
  /// Stable module or entry-point name.
  pub name: String,
  /// Compiled bytecode and all of its nested function codes.
  pub code: Chunk,
}

/// Errors produced while encoding or decoding bytecode bundles.
#[derive(Debug, Error)]
pub enum BundleError {
  /// The input does not begin with the Blitz bytecode bundle signature.
  #[error("invalid Blitz bytecode bundle header")]
  InvalidHeader,
  /// The container version is not supported by this engine.
  #[error("unsupported bytecode bundle version {0}")]
  UnsupportedVersion(u16),
  /// The encoded payload exceeds the codec's defensive size limit.
  #[error("bytecode bundle is {actual} bytes; maximum supported size is {maximum} bytes")]
  TooLarge {
    /// Observed byte count.
    actual: usize,
    /// Maximum accepted byte count.
    maximum: usize,
  },
  /// The envelope's payload length does not match the supplied bytes.
  #[error("bytecode bundle payload length does not match its header")]
  LengthMismatch,
  /// The payload checksum indicates corruption.
  #[error("bytecode bundle checksum mismatch")]
  ChecksumMismatch,
  /// Postcard could not encode or decode the payload.
  #[error("invalid bytecode bundle payload: {0}")]
  Codec(#[from] postcard::Error),
  /// A serialized field is invalid or cannot be represented on this target.
  #[error("invalid bytecode bundle data: {0}")]
  InvalidData(String),
  /// A reconstructed code failed structural bytecode verification.
  #[error("invalid chunk {name:?}: {reason}")]
  InvalidChunk {
    /// Entry name or code-table label used for diagnostics.
    name: String,
    /// Structural verification failure.
    reason: String,
  },
}

/// Encode named codes into the deterministic bytecode bundle format.
///
/// Chunks are verified before encoding. The same logical input, including its
/// entry order and `Arc` sharing, always produces the same bytes.
///
/// # Errors
///
/// Returns an error if a code is invalid, a field exceeds the portable wire
/// representation, or the resulting bundle exceeds the size limit.
pub fn encode(entries: &[NamedChunk]) -> Result<Vec<u8>, BundleError> {
  let mut encoder = Encoder::default();
  let mut wire_entries = Vec::with_capacity(entries.len());

  for entry in entries {
    entry
      .code
      .verify()
      .map_err(|reason| BundleError::InvalidChunk {
        name: entry.name.clone(),
        reason,
      })?;
    wire_entries.push(WireNamedChunk {
      name: entry.name.clone(),
      code: encoder.add_root(&entry.code)?,
    });
  }

  let bundle = WireBundle {
    sources: encoder.sources,
    codes: encoder
      .codes
      .into_iter()
      .enumerate()
      .map(|(index, code)| {
        code.ok_or_else(|| {
          BundleError::InvalidData(format!("code table entry {index} was not completed"))
        })
      })
      .collect::<Result<_, _>>()?,
    entries: wire_entries,
  };
  let payload = postcard::to_allocvec(&bundle)?;
  frame_payload(&payload)
}

/// Decode and verify named codes from a bytecode bundle.
///
/// The decoder validates the signature, exact format version, payload length,
/// checksum, all table references, nesting depth, portable integer widths, and
/// every reconstructed [`Chunk`]. Trailing bytes are rejected.
///
/// # Errors
///
/// Returns an error for unsupported versions, corruption, malformed fields,
/// excessive input, cyclic/deep code graphs, or failed code verification.
pub fn decode(bytes: &[u8]) -> Result<Vec<NamedChunk>, BundleError> {
  if bytes.len() > MAX_BUNDLE_BYTES {
    return Err(BundleError::TooLarge {
      actual: bytes.len(),
      maximum: MAX_BUNDLE_BYTES,
    });
  }
  if bytes.len() < HEADER_LEN || &bytes[..MAGIC.len()] != MAGIC {
    return Err(BundleError::InvalidHeader);
  }

  let version = u16::from_le_bytes([bytes[8], bytes[9]]);
  if version != BYTECODE_BUNDLE_VERSION {
    return Err(BundleError::UnsupportedVersion(version));
  }

  let payload_len = u64::from_le_bytes(
    bytes[10..18]
      .try_into()
      .map_err(|_| BundleError::InvalidHeader)?,
  );
  let payload_len = usize::try_from(payload_len)
    .map_err(|_| BundleError::InvalidData("payload length does not fit usize".into()))?;
  if payload_len > MAX_BUNDLE_BYTES - HEADER_LEN
    || bytes.len().checked_sub(HEADER_LEN) != Some(payload_len)
  {
    return Err(BundleError::LengthMismatch);
  }

  let expected_checksum = u32::from_le_bytes(
    bytes[18..22]
      .try_into()
      .map_err(|_| BundleError::InvalidHeader)?,
  );
  let payload = &bytes[HEADER_LEN..];
  if crc32fast::hash(payload) != expected_checksum {
    return Err(BundleError::ChecksumMismatch);
  }

  let wire: WireBundle = postcard::from_bytes(payload)?;
  let sources = wire.sources.into_iter().map(Arc::new).collect();
  let mut decoder = Decoder {
    codes: wire.codes,
    sources,
    states: Vec::new(),
    decoded: Vec::new(),
  };
  decoder
    .states
    .resize(decoder.codes.len(), DecodeState::New);
  decoder.decoded.resize(decoder.codes.len(), None);

  for index in 0..decoder.codes.len() {
    decoder.build_code(index, 0)?;
  }

  wire
    .entries
    .into_iter()
    .map(|entry| {
      let index = index_from_u32(entry.code, decoder.decoded.len(), "entry code")?;
      let code = decoder.decoded[index]
        .as_ref()
        .ok_or_else(|| BundleError::InvalidData(format!("missing decoded code {index}")))?;
      code
        .verify()
        .map_err(|reason| BundleError::InvalidChunk {
          name: entry.name.clone(),
          reason,
        })?;
      Ok(NamedChunk {
        name: entry.name,
        code: code.as_ref().clone(),
      })
    })
    .collect()
}

fn frame_payload(payload: &[u8]) -> Result<Vec<u8>, BundleError> {
  let total = HEADER_LEN
    .checked_add(payload.len())
    .ok_or_else(|| BundleError::InvalidData("bundle length overflow".into()))?;
  if total > MAX_BUNDLE_BYTES {
    return Err(BundleError::TooLarge {
      actual: total,
      maximum: MAX_BUNDLE_BYTES,
    });
  }
  let payload_len = u64::try_from(payload.len())
    .map_err(|_| BundleError::InvalidData("payload length does not fit u64".into()))?;
  let mut bytes = Vec::with_capacity(total);
  bytes.extend_from_slice(MAGIC);
  bytes.extend_from_slice(&BYTECODE_BUNDLE_VERSION.to_le_bytes());
  bytes.extend_from_slice(&payload_len.to_le_bytes());
  bytes.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
  bytes.extend_from_slice(payload);
  Ok(bytes)
}

#[derive(Serialize, Deserialize)]
struct WireBundle {
  sources: Vec<String>,
  codes: Vec<WireChunk>,
  entries: Vec<WireNamedChunk>,
}

#[derive(Serialize, Deserialize)]
struct WireNamedChunk {
  name: String,
  code: u32,
}

#[derive(Clone, Serialize, Deserialize)]
struct WireChunk {
  version: u8,
  code: Vec<u32>,
  constants: Vec<WireConstant>,
  functions: Vec<WireFunctionProto>,
  lines: Vec<u32>,
  source: u32,
  root_slot_types: Vec<Option<WireType>>,
  root_slot_names: Vec<Option<String>>,
  root_lexical_tdz: Vec<bool>,
  root_slot_immutable: Vec<bool>,
  spill_slot_count: u16,
  value_spill_slot_count: u16,
  root_strict: bool,
  allow_new_target: bool,
  allow_super: bool,
  finally_handlers: Vec<WireFinallyHandler>,
}

#[derive(Clone, Serialize, Deserialize)]
struct WireFinallyHandler {
  push_pc: u64,
  finalizer_pc: u64,
  return_slot: u64,
  did_return_slot: u64,
}

#[derive(Clone, Serialize, Deserialize)]
struct WireFunctionProto {
  name: String,
  span_start: u32,
  span_end: u32,
  arity: u64,
  length: u64,
  code: u32,
  slot_types: Vec<Option<WireType>>,
  slot_names: Vec<Option<String>>,
  lexical_tdz: Vec<bool>,
  slot_immutable: Vec<bool>,
  slot_sloppy_immutable_silent: Vec<bool>,
  upvalues: Vec<WireUpvalueSpec>,
  has_rest: bool,
  has_prototype_property: bool,
  captures_this: bool,
  arguments_slot: Option<u64>,
  self_binding_slot: Option<u64>,
  has_simple_parameter_list: bool,
  return_type: Option<WireType>,
  is_strict: bool,
  is_constructor: bool,
  is_derived_constructor: bool,
  is_class_constructor: bool,
  is_async: bool,
  is_generator: bool,
  ic_table: Vec<WireICEntry>,
}

#[derive(Clone, Serialize, Deserialize)]
struct WireUpvalueSpec {
  is_local: bool,
  index: u16,
  mirror_global_name: Option<String>,
  immutable: bool,
  sloppy_immutable_silent: bool,
}

#[derive(Clone, Serialize, Deserialize)]
struct WireICEntry {
  key: u64,
  value: u64,
  generation: u32,
  ic_type: u8,
  reserved: u8,
}

#[derive(Clone, Serialize, Deserialize)]
enum WireConstant {
  Uninitialized,
  Number(f64),
  String(Vec<u16>),
  Bool(bool),
  Undefined,
  BigInt(WireSign, Vec<u8>),
  Symbol(u64, Option<Vec<u16>>),
  Null,
}

#[derive(Clone, Copy, Serialize, Deserialize)]
enum WireSign {
  None,
  Plus,
  Minus,
}

#[derive(Clone, Serialize, Deserialize)]
enum WireType {
  Never,
  Number,
  String,
  Boolean,
  Null,
  Undefined,
  Void,
  Any,
  Unknown,
  Array(Box<WireType>),
  Tuple(Vec<WireType>),
  Rest(Box<WireType>),
  Function(Vec<WireType>, Box<WireType>),
  Object(Vec<(String, WireType)>),
  Builtin(WireBuiltinType),
  TypeReference {
    name: String,
    arguments: Vec<WireType>,
  },
  Union(Vec<WireType>),
  Intersection(Vec<WireType>),
  Literal(WireLiteralType),
  Conditional {
    check: Box<WireType>,
    extends: Box<WireType>,
    true_type: Box<WireType>,
    false_type: Box<WireType>,
    distributive_parameter: Option<String>,
  },
  KeyOf(Box<WireType>),
  IndexedAccess {
    object: Box<WireType>,
    index: Box<WireType>,
  },
  Infer(String),
  Mapped {
    parameter: String,
    constraint: Box<WireType>,
    value: Box<WireType>,
  },
  TemplateLiteral {
    quasis: Vec<String>,
    substitutions: Vec<WireType>,
  },
}

#[derive(Clone, Serialize, Deserialize)]
enum WireLiteralType {
  String(String),
  Number(u64),
  Boolean(bool),
}

#[derive(Clone, Serialize, Deserialize)]
enum WireBuiltinType {
  Undefined,
  Null,
  Boolean,
  Number,
  String,
  Symbol,
  BigInt,
  Object,
  Function,
  Array,
}

#[derive(Default)]
struct Encoder {
  sources: Vec<String>,
  source_ids: HashMap<usize, u32>,
  codes: Vec<Option<WireChunk>>,
  code_ids: HashMap<usize, u32>,
}

impl Encoder {
  fn add_root(
    &mut self,
    code: &Chunk,
  ) -> Result<u32, BundleError> {
    let index = wire_index(self.codes.len(), "code")?;
    self.codes.push(None);
    let encoded = self.encode_code(code)?;
    self.codes[index as usize] = Some(encoded);
    Ok(index)
  }

  fn add_nested(
    &mut self,
    code: &Arc<Chunk>,
  ) -> Result<u32, BundleError> {
    let identity = Arc::as_ptr(code) as usize;
    if let Some(index) = self.code_ids.get(&identity) {
      return Ok(*index);
    }
    let index = wire_index(self.codes.len(), "code")?;
    self.code_ids.insert(identity, index);
    self.codes.push(None);
    let encoded = self.encode_code(code)?;
    self.codes[index as usize] = Some(encoded);
    Ok(index)
  }

  fn source_index(
    &mut self,
    source: &Arc<String>,
  ) -> Result<u32, BundleError> {
    let identity = Arc::as_ptr(source) as usize;
    if let Some(index) = self.source_ids.get(&identity) {
      return Ok(*index);
    }
    let index = wire_index(self.sources.len(), "source")?;
    self.source_ids.insert(identity, index);
    self.sources.push(source.as_ref().clone());
    Ok(index)
  }

  fn encode_code(
    &mut self,
    code: &Chunk,
  ) -> Result<WireChunk, BundleError> {
    Ok(WireChunk {
      version: code.version as u8,
      code: code.words.clone(),
      constants: code
        .constants
        .iter()
        .map(WireConstant::from)
        .collect(),
      functions: code
        .functions
        .iter()
        .map(|function| self.encode_function(function))
        .collect::<Result<_, _>>()?,
      lines: code.lines.clone(),
      source: self.source_index(&code.source_text)?,
      root_slot_types: encode_optional_types(&code.root_slot_types),
      root_slot_names: code.root_slot_names.clone(),
      root_lexical_tdz: code.root_lexical_tdz.clone(),
      root_slot_immutable: code.root_slot_immutable.clone(),
      spill_slot_count: code.spill_slot_count,
      value_spill_slot_count: code.value_spill_slot_count,
      root_strict: code.root_strict,
      allow_new_target: code.allow_new_target,
      allow_super: code.allow_super,
      finally_handlers: code
        .finally_handlers
        .iter()
        .map(|handler| WireFinallyHandler {
          push_pc: handler.push_pc as u64,
          finalizer_pc: handler.finalizer_pc as u64,
          return_slot: handler.return_slot as u64,
          did_return_slot: handler.did_return_slot as u64,
        })
        .collect(),
    })
  }

  fn encode_function(
    &mut self,
    function: &Function,
  ) -> Result<WireFunctionProto, BundleError> {
    Ok(WireFunctionProto {
      name: function.name.clone(),
      span_start: function.span.start,
      span_end: function.span.end,
      arity: function.arity as u64,
      length: function.length as u64,
      code: self.add_nested(&function.code)?,
      slot_types: encode_optional_types(&function.slot_types),
      slot_names: function.slot_names.clone(),
      lexical_tdz: function.lexical_tdz.clone(),
      slot_immutable: function.slot_immutable.clone(),
      slot_sloppy_immutable_silent: function.slot_sloppy_immutable_silent.clone(),
      upvalues: function
        .upvalues
        .iter()
        .map(|upvalue| WireUpvalueSpec {
          is_local: upvalue.is_local,
          index: upvalue.index,
          mirror_global_name: upvalue.mirror_global_name.clone(),
          immutable: upvalue.immutable,
          sloppy_immutable_silent: upvalue.sloppy_immutable_silent,
        })
        .collect(),
      has_rest: function.has_rest,
      has_prototype_property: function.has_prototype_property,
      captures_this: function.captures_this,
      arguments_slot: function.arguments_slot.map(|value| value as u64),
      self_binding_slot: function
        .self_binding_slot
        .map(|value| value as u64),
      has_simple_parameter_list: function.has_simple_parameter_list,
      return_type: function.return_type.as_ref().map(WireType::from),
      is_strict: function.is_strict,
      is_constructor: function.is_constructor,
      is_derived_constructor: function.is_derived_constructor,
      is_class_constructor: function.is_class_constructor,
      is_async: function.is_async,
      is_generator: function.is_generator,
      ic_table: Vec::new(),
    })
  }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DecodeState {
  New,
  Building,
  Complete,
}

struct Decoder {
  codes: Vec<WireChunk>,
  sources: Vec<Arc<String>>,
  states: Vec<DecodeState>,
  decoded: Vec<Option<Arc<Chunk>>>,
}

impl Decoder {
  fn build_code(
    &mut self,
    index: usize,
    depth: usize,
  ) -> Result<Arc<Chunk>, BundleError> {
    if depth > MAX_CHUNK_DEPTH {
      return Err(BundleError::InvalidData(format!(
        "code nesting exceeds {MAX_CHUNK_DEPTH} levels"
      )));
    }
    let state = self
      .states
      .get(index)
      .copied()
      .ok_or_else(|| BundleError::InvalidData(format!("code index {index} is out of bounds")))?;
    if state == DecodeState::Complete {
      return self.decoded[index]
        .as_ref()
        .cloned()
        .ok_or_else(|| BundleError::InvalidData(format!("missing decoded code {index}")));
    }
    if state == DecodeState::Building {
      return Err(BundleError::InvalidData(
        "cyclic nested code reference".into(),
      ));
    }

    self.states[index] = DecodeState::Building;
    let wire = self.codes[index].clone();
    let source_index = index_from_u32(wire.source, self.sources.len(), "source")?;
    let functions = wire
      .functions
      .into_iter()
      .map(|function| self.decode_function(function, depth + 1))
      .collect::<Result<_, _>>()?;
    let version = match wire.version {
      value if value == BytecodeVersion::RiscV2 as u8 => BytecodeVersion::RiscV2,
      value => {
        return Err(BundleError::InvalidData(format!(
          "unsupported code bytecode version {value}"
        )));
      }
    };
    let code = Arc::new(Chunk {
      version,
      words: wire.code,
      constants: wire
        .constants
        .into_iter()
        .map(Constant::from)
        .collect(),
      functions,
      lines: wire.lines,
      source_text: Arc::clone(&self.sources[source_index]),
      root_slot_types: decode_optional_types(wire.root_slot_types)?,
      root_slot_names: wire.root_slot_names,
      root_lexical_tdz: wire.root_lexical_tdz,
      root_slot_immutable: wire.root_slot_immutable,
      root_function_declarations: Vec::new(),
      spill_slot_count: wire.spill_slot_count,
      value_spill_slot_count: wire.value_spill_slot_count,
      root_strict: wire.root_strict,
      allow_new_target: wire.allow_new_target,
      allow_super: wire.allow_super,
      finally_handlers: wire
        .finally_handlers
        .into_iter()
        .map(|handler| {
          Ok(FinallyHandler {
            push_pc: usize_from_u64(handler.push_pc, "finally push instruction")?,
            finalizer_pc: usize_from_u64(handler.finalizer_pc, "finalizer instruction")?,
            return_slot: usize_from_u64(handler.return_slot, "finally return slot")?,
            did_return_slot: usize_from_u64(handler.did_return_slot, "finally did-return slot")?,
          })
        })
        .collect::<Result<_, BundleError>>()?,
    });
    self.decoded[index] = Some(Arc::clone(&code));
    self.states[index] = DecodeState::Complete;
    Ok(code)
  }

  fn decode_function(
    &mut self,
    wire: WireFunctionProto,
    depth: usize,
  ) -> Result<Function, BundleError> {
    let code_index = index_from_u32(wire.code, self.codes.len(), "nested code")?;
    Ok(Function {
      name: wire.name,
      span: Span::new(wire.span_start, wire.span_end),
      arity: usize_from_u64(wire.arity, "function arity")?,
      length: usize_from_u64(wire.length, "function length")?,
      code: self.build_code(code_index, depth)?,
      slot_types: decode_optional_types(wire.slot_types)?,
      slot_names: wire.slot_names,
      lexical_tdz: wire.lexical_tdz,
      slot_immutable: wire.slot_immutable,
      slot_sloppy_immutable_silent: wire.slot_sloppy_immutable_silent,
      upvalues: wire
        .upvalues
        .into_iter()
        .map(|upvalue| UpvalueSpec {
          is_local: upvalue.is_local,
          index: upvalue.index,
          mirror_global_name: upvalue.mirror_global_name,
          immutable: upvalue.immutable,
          sloppy_immutable_silent: upvalue.sloppy_immutable_silent,
        })
        .collect(),
      has_rest: wire.has_rest,
      has_prototype_property: wire.has_prototype_property,
      captures_this: wire.captures_this,
      arguments_slot: wire
        .arguments_slot
        .map(|value| usize_from_u64(value, "arguments slot"))
        .transpose()?,
      self_binding_slot: wire
        .self_binding_slot
        .map(|value| usize_from_u64(value, "self-binding slot"))
        .transpose()?,
      has_simple_parameter_list: wire.has_simple_parameter_list,
      return_type: wire.return_type.map(Type::try_from).transpose()?,
      is_strict: wire.is_strict,
      is_constructor: wire.is_constructor,
      is_derived_constructor: wire.is_derived_constructor,
      is_class_constructor: wire.is_class_constructor,
      is_async: wire.is_async,
      is_generator: wire.is_generator,
    })
  }
}

fn wire_index(
  length: usize,
  kind: &str,
) -> Result<u32, BundleError> {
  u32::try_from(length)
    .map_err(|_| BundleError::InvalidData(format!("too many {kind} table entries")))
}

fn index_from_u32(
  index: u32,
  length: usize,
  kind: &str,
) -> Result<usize, BundleError> {
  let index = index as usize;
  if index >= length {
    Err(BundleError::InvalidData(format!(
      "{kind} index {index} is out of bounds"
    )))
  } else {
    Ok(index)
  }
}

fn usize_from_u64(
  value: u64,
  field: &str,
) -> Result<usize, BundleError> {
  usize::try_from(value)
    .map_err(|_| BundleError::InvalidData(format!("{field} does not fit usize")))
}

fn encode_optional_types(types: &[Option<Type>]) -> Vec<Option<WireType>> {
  types
    .iter()
    .map(|value| value.as_ref().map(WireType::from))
    .collect()
}

fn decode_optional_types(types: Vec<Option<WireType>>) -> Result<Vec<Option<Type>>, BundleError> {
  types
    .into_iter()
    .map(|value| value.map(Type::try_from).transpose())
    .collect()
}

impl From<&Constant> for WireConstant {
  fn from(value: &Constant) -> Self {
    match value {
      Constant::Uninitialized => Self::Uninitialized,
      Constant::Number(value) => Self::Number(*value),
      Constant::String(value) => Self::String(value.as_slice().to_vec()),
      Constant::Bool(value) => Self::Bool(*value),
      Constant::Undefined => Self::Undefined,
      Constant::BigInt(value) => {
        let (sign, bytes) = value.to_bytes_le();
        Self::BigInt(WireSign::from(sign), bytes)
      }
      Constant::Symbol(id, description) => Self::Symbol(
        id.as_u64(),
        description
          .as_ref()
          .map(|value| value.as_slice().to_vec()),
      ),
      Constant::Null => Self::Null,
    }
  }
}

impl From<WireConstant> for Constant {
  fn from(value: WireConstant) -> Self {
    match value {
      WireConstant::Uninitialized => Self::Uninitialized,
      WireConstant::Number(value) => Self::Number(value),
      WireConstant::String(value) => Self::String(value.into()),
      WireConstant::Bool(value) => Self::Bool(value),
      WireConstant::Undefined => Self::Undefined,
      WireConstant::BigInt(sign, bytes) => Self::BigInt(BigInt::from_bytes_le(sign.into(), &bytes)),
      WireConstant::Symbol(id, description) => {
        Self::Symbol(SymbolId::well_known(id), description.map(Into::into))
      }
      WireConstant::Null => Self::Null,
    }
  }
}

impl From<Sign> for WireSign {
  fn from(value: Sign) -> Self {
    match value {
      Sign::NoSign => Self::None,
      Sign::Plus => Self::Plus,
      Sign::Minus => Self::Minus,
    }
  }
}

impl From<WireSign> for Sign {
  fn from(value: WireSign) -> Self {
    match value {
      WireSign::None => Self::NoSign,
      WireSign::Plus => Self::Plus,
      WireSign::Minus => Self::Minus,
    }
  }
}

impl From<&Type> for WireType {
  fn from(value: &Type) -> Self {
    match value {
      Type::Never => Self::Never,
      Type::Number => Self::Number,
      Type::String => Self::String,
      Type::Boolean => Self::Boolean,
      Type::Null => Self::Null,
      Type::Undefined => Self::Undefined,
      Type::Void => Self::Void,
      Type::Any => Self::Any,
      Type::Unknown => Self::Unknown,
      Type::Array(value) => Self::Array(Box::new(Self::from(value.as_ref()))),
      Type::Tuple(values) => Self::Tuple(values.iter().map(Self::from).collect()),
      Type::Rest(value) => Self::Rest(Box::new(Self::from(value.as_ref()))),
      Type::Function(parameters, result) => Self::Function(
        parameters.iter().map(Self::from).collect(),
        Box::new(Self::from(result.as_ref())),
      ),
      Type::Object(properties) => {
        let mut properties = properties
          .iter()
          .map(|(name, value)| (name.clone(), Self::from(value)))
          .collect::<Vec<_>>();
        properties.sort_by(|left, right| left.0.cmp(&right.0));
        Self::Object(properties)
      }
      Type::Builtin(value) => Self::Builtin(WireBuiltinType::from(value)),
      Type::TypeReference { name, arguments } => Self::TypeReference {
        name: name.clone(),
        arguments: arguments.iter().map(Self::from).collect(),
      },
      Type::Union(values) => Self::Union(values.iter().map(Self::from).collect()),
      Type::Intersection(values) => Self::Intersection(values.iter().map(Self::from).collect()),
      Type::Literal(value) => Self::Literal(WireLiteralType::from(value)),
      Type::Conditional {
        check,
        extends,
        true_type,
        false_type,
        distributive_parameter,
      } => Self::Conditional {
        check: Box::new(Self::from(check.as_ref())),
        extends: Box::new(Self::from(extends.as_ref())),
        true_type: Box::new(Self::from(true_type.as_ref())),
        false_type: Box::new(Self::from(false_type.as_ref())),
        distributive_parameter: distributive_parameter.clone(),
      },
      Type::KeyOf(value) => Self::KeyOf(Box::new(Self::from(value.as_ref()))),
      Type::IndexedAccess { object, index } => Self::IndexedAccess {
        object: Box::new(Self::from(object.as_ref())),
        index: Box::new(Self::from(index.as_ref())),
      },
      Type::Infer(value) => Self::Infer(value.clone()),
      Type::Mapped {
        parameter,
        constraint,
        value,
      } => Self::Mapped {
        parameter: parameter.clone(),
        constraint: Box::new(Self::from(constraint.as_ref())),
        value: Box::new(Self::from(value.as_ref())),
      },
      Type::TemplateLiteral {
        quasis,
        substitutions,
      } => Self::TemplateLiteral {
        quasis: quasis.clone(),
        substitutions: substitutions.iter().map(Self::from).collect(),
      },
    }
  }
}

impl TryFrom<WireType> for Type {
  type Error = BundleError;

  fn try_from(value: WireType) -> Result<Self, Self::Error> {
    Ok(match value {
      WireType::Never => Self::Never,
      WireType::Number => Self::Number,
      WireType::String => Self::String,
      WireType::Boolean => Self::Boolean,
      WireType::Null => Self::Null,
      WireType::Undefined => Self::Undefined,
      WireType::Void => Self::Void,
      WireType::Any => Self::Any,
      WireType::Unknown => Self::Unknown,
      WireType::Array(value) => Self::Array(Box::new(Self::try_from(*value)?)),
      WireType::Tuple(values) => Self::Tuple(
        values
          .into_iter()
          .map(Self::try_from)
          .collect::<Result<_, _>>()?,
      ),
      WireType::Rest(value) => Self::Rest(Box::new(Self::try_from(*value)?)),
      WireType::Function(parameters, result) => Self::Function(
        parameters
          .into_iter()
          .map(Self::try_from)
          .collect::<Result<_, _>>()?,
        Box::new(Self::try_from(*result)?),
      ),
      WireType::Object(properties) => {
        let expected_len = properties.len();
        let properties = properties
          .into_iter()
          .map(|(name, value)| Ok((name, Self::try_from(value)?)))
          .collect::<Result<HashMap<_, _>, BundleError>>()?;
        if properties.len() != expected_len {
          return Err(BundleError::InvalidData(
            "object type contains duplicate property names".into(),
          ));
        }
        Self::Object(properties)
      }
      WireType::Builtin(value) => Self::Builtin(value.into()),
      WireType::TypeReference { name, arguments } => Self::TypeReference {
        name,
        arguments: arguments
          .into_iter()
          .map(Self::try_from)
          .collect::<Result<_, _>>()?,
      },
      WireType::Union(values) => Self::Union(
        values
          .into_iter()
          .map(Self::try_from)
          .collect::<Result<_, _>>()?,
      ),
      WireType::Intersection(values) => Self::Intersection(
        values
          .into_iter()
          .map(Self::try_from)
          .collect::<Result<_, _>>()?,
      ),
      WireType::Literal(value) => Self::Literal(value.into()),
      WireType::Conditional {
        check,
        extends,
        true_type,
        false_type,
        distributive_parameter,
      } => Self::Conditional {
        check: Box::new(Self::try_from(*check)?),
        extends: Box::new(Self::try_from(*extends)?),
        true_type: Box::new(Self::try_from(*true_type)?),
        false_type: Box::new(Self::try_from(*false_type)?),
        distributive_parameter,
      },
      WireType::KeyOf(value) => Self::KeyOf(Box::new(Self::try_from(*value)?)),
      WireType::IndexedAccess { object, index } => Self::IndexedAccess {
        object: Box::new(Self::try_from(*object)?),
        index: Box::new(Self::try_from(*index)?),
      },
      WireType::Infer(value) => Self::Infer(value),
      WireType::Mapped {
        parameter,
        constraint,
        value,
      } => Self::Mapped {
        parameter,
        constraint: Box::new(Self::try_from(*constraint)?),
        value: Box::new(Self::try_from(*value)?),
      },
      WireType::TemplateLiteral {
        quasis,
        substitutions,
      } => Self::TemplateLiteral {
        quasis,
        substitutions: substitutions
          .into_iter()
          .map(Self::try_from)
          .collect::<Result<_, _>>()?,
      },
    })
  }
}

impl From<&LiteralType> for WireLiteralType {
  fn from(value: &LiteralType) -> Self {
    match value {
      LiteralType::String(value) => Self::String(value.clone()),
      LiteralType::Number(value) => Self::Number(*value),
      LiteralType::Boolean(value) => Self::Boolean(*value),
    }
  }
}

impl From<WireLiteralType> for LiteralType {
  fn from(value: WireLiteralType) -> Self {
    match value {
      WireLiteralType::String(value) => Self::String(value),
      WireLiteralType::Number(value) => Self::Number(value),
      WireLiteralType::Boolean(value) => Self::Boolean(value),
    }
  }
}

impl From<&BuiltinType> for WireBuiltinType {
  fn from(value: &BuiltinType) -> Self {
    match value {
      BuiltinType::Undefined => Self::Undefined,
      BuiltinType::Null => Self::Null,
      BuiltinType::Boolean => Self::Boolean,
      BuiltinType::Number => Self::Number,
      BuiltinType::String => Self::String,
      BuiltinType::Symbol => Self::Symbol,
      BuiltinType::BigInt => Self::BigInt,
      BuiltinType::Object => Self::Object,
      BuiltinType::Function => Self::Function,
      BuiltinType::Array => Self::Array,
    }
  }
}

impl From<WireBuiltinType> for BuiltinType {
  fn from(value: WireBuiltinType) -> Self {
    match value {
      WireBuiltinType::Undefined => Self::Undefined,
      WireBuiltinType::Null => Self::Null,
      WireBuiltinType::Boolean => Self::Boolean,
      WireBuiltinType::Number => Self::Number,
      WireBuiltinType::String => Self::String,
      WireBuiltinType::Symbol => Self::Symbol,
      WireBuiltinType::BigInt => Self::BigInt,
      WireBuiltinType::Object => Self::Object,
      WireBuiltinType::Function => Self::Function,
      WireBuiltinType::Array => Self::Array,
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use super::{decode as decode_bytecode_bundle, encode as encode_bytecode_bundle};
  use crate::bytecode::Opcode;

  fn populated_bundle() -> Vec<NamedChunk> {
    let source = Arc::new(crate::js!("function nested() { return undefined; }").to_string());
    let mut nested = Chunk::new(Arc::clone(&source));
    nested.write_instr(Opcode::PushHandler as u32, 7);
    nested.write_instr(Opcode::Return as u32, 7);
    nested.constants = vec![
      Constant::Uninitialized,
      Constant::Number(f64::from_bits(0x7ff8_0000_0000_0042)),
      Constant::String("constant".into()),
      Constant::Bool(true),
      Constant::Undefined,
      Constant::BigInt(BigInt::from(-123456789_i64)),
      Constant::Symbol(SymbolId::well_known(77), Some("symbol".into())),
      Constant::Null,
    ];
    nested.root_slot_types = vec![Some(Type::Tuple(vec![Type::Number, Type::String]))];
    nested.root_slot_names = vec![Some("nested_slot".into())];
    nested.root_lexical_tdz = vec![true];
    nested.root_slot_immutable = vec![true];
    nested.spill_slot_count = 4;
    nested.value_spill_slot_count = 3;
    nested.root_strict = true;
    nested.allow_new_target = true;
    nested.allow_super = true;
    nested.finally_handlers.push(FinallyHandler {
      push_pc: 0,
      finalizer_pc: 1,
      return_slot: 2,
      did_return_slot: 3,
    });

    let mut properties = HashMap::new();
    properties.insert("z".into(), Type::Boolean);
    properties.insert("a".into(), Type::Array(Box::new(Type::String)));
    let mut root = Chunk::new(source);
    root.write_instr(Opcode::Undefined as u32, 1);
    root.write_instr(Opcode::Return as u32, 1);
    root.root_slot_types = vec![Some(Type::Object(properties)), None];
    root.root_slot_names = vec![Some("root".into()), None];
    root.root_lexical_tdz = vec![false, true];
    root.root_slot_immutable = vec![false, true];
    root.functions.push(Function {
      name: "nested".into(),
      span: Span::new(0, 42),
      arity: 2,
      length: 1,
      code: Arc::new(nested),
      slot_types: vec![
        Some(Type::Literal(LiteralType::String("x".into()))),
        None,
        None,
        None,
      ],
      slot_names: vec![Some("arg".into()), None, None, None],
      lexical_tdz: vec![false, false, false, false],
      slot_immutable: vec![true, false, false, false],
      slot_sloppy_immutable_silent: vec![true, false, false, false],
      upvalues: vec![UpvalueSpec {
        is_local: true,
        index: 1,
        mirror_global_name: Some("global".into()),
        immutable: true,
        sloppy_immutable_silent: true,
      }],
      has_rest: true,
      has_prototype_property: true,
      captures_this: true,
      arguments_slot: Some(0),
      self_binding_slot: Some(1),
      has_simple_parameter_list: false,
      return_type: Some(Type::Union(vec![Type::Undefined, Type::Number])),
      is_strict: true,
      is_constructor: true,
      is_derived_constructor: true,
      is_class_constructor: true,
      is_async: true,
      is_generator: true,
    });

    vec![NamedChunk {
      name: "module.js".into(),
      code: root,
    }]
  }

  #[test]
  fn bundle_round_trip_preserves_all_fields_and_is_deterministic() {
    let entries = populated_bundle();
    let first = encode_bytecode_bundle(&entries).expect("encode bundle");
    let second = encode_bytecode_bundle(&entries).expect("encode bundle again");
    assert_eq!(first, second);

    let decoded = decode_bytecode_bundle(&first).expect("decode bundle");
    assert_eq!(decoded.len(), 1);
    assert_eq!(decoded[0].name, "module.js");
    let root = &decoded[0].code;
    let function = &root.functions[0];
    assert_eq!(function.span, Span::new(0, 42));
    assert_eq!(function.arity, 2);
    assert_eq!(function.length, 1);
    assert_eq!(
      function.upvalues[0].mirror_global_name.as_deref(),
      Some("global")
    );
    assert_eq!(function.code.constants.len(), 8);
    assert_eq!(function.code.value_spill_slot_count, 3);
    assert_eq!(function.code.finally_handlers[0].did_return_slot, 3);
    assert!(Arc::ptr_eq(&root.source_text, &function.code.source_text));
    assert_eq!(
      encode_bytecode_bundle(&decoded).expect("re-encode decoded bundle"),
      first
    );
  }

  #[test]
  fn rejects_corruption() {
    let mut bytes = encode_bytecode_bundle(&populated_bundle()).expect("encode bundle");
    let last = bytes.len() - 1;
    bytes[last] ^= 0x80;
    assert!(matches!(
      decode_bytecode_bundle(&bytes),
      Err(BundleError::ChecksumMismatch)
    ));
  }

  #[test]
  fn rejects_unknown_format_version() {
    let mut bytes = encode_bytecode_bundle(&populated_bundle()).expect("encode bundle");
    bytes[8..10].copy_from_slice(&(BYTECODE_BUNDLE_VERSION + 1).to_le_bytes());
    assert!(matches!(
      decode_bytecode_bundle(&bytes),
      Err(BundleError::UnsupportedVersion(version))
        if version == BYTECODE_BUNDLE_VERSION + 1
    ));
  }

  #[test]
  fn verifies_code_after_decode() {
    let bytes = encode_bytecode_bundle(&populated_bundle()).expect("encode bundle");
    let mut wire: WireBundle =
      postcard::from_bytes(&bytes[HEADER_LEN..]).expect("decode private wire payload");
    wire.codes[0].code[0] = 0xff;
    let payload = postcard::to_allocvec(&wire).expect("encode private wire payload");
    let bytes = frame_payload(&payload).expect("frame private wire payload");

    assert!(matches!(
      decode_bytecode_bundle(&bytes),
      Err(BundleError::InvalidChunk { reason, .. }) if reason.contains("invalid opcode")
    ));
  }
}
