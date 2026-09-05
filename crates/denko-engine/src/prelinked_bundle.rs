// SPDX-License-Identifier: MPL-2.0
//! Deterministic wire encoding for host-neutral prelinked module graphs.
//!
//! This format is deliberately separate from [`crate::bytecode_bundle`]. It
//! carries graph/linking metadata as well as the complete code tree. Version
//! Version 3 stores source text once per module instead of repeating it in every
//! nested code, version 4 stores function lengths, and version 5 widens
//! captured local indices. Its decoder bounds every length before allocating
//! and retains read compatibility with version 2 bundles.
//! Decoded graphs are passed through [`PrelinkedModuleGraph::validate`] before
//! they are returned.

use crate::bytecode::{BytecodeVersion, Chunk, Constant, FinallyHandler, Function, UpvalueSpec};
use crate::prelinked::{
  PrelinkedExport, PrelinkedGraphError, PrelinkedImport, PrelinkedModule, PrelinkedModuleGraph,
};
use crate::typecheck::{BuiltinType, LiteralType, Type};
use crate::value::SymbolId;
use num_bigint::{BigInt, Sign};
use oxc_span::Span;
use std::collections::HashMap;
use std::sync::Arc;
use thiserror::Error;

/// Current version of the prelinked graph wire envelope.
pub const PRELINKED_GRAPH_WIRE_VERSION: u16 = 6;

const SOURCE_TEXT_WIRE_VERSION: u16 = 2;

const MAGIC: &[u8; 8] = b"BLITZPG\0";
const HEADER_LEN: usize = MAGIC.len() + 2 + 8 + 4;
const MAX_WIRE_BYTES: usize = 512 * 1024 * 1024;
const MAX_MODULES: usize = 65_536;
const MAX_BINDINGS: usize = 1_000_000;
const MAX_DEPENDENCIES: usize = 4_000_000;
const MAX_BINDING_ENTRIES: usize = 8_000_000;
const MAX_PUBLIC_ROOTS: usize = 65_536;
const MAX_NAME_BYTES: usize = 64 * 1024;
const MAX_TOTAL_NAME_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_SOURCE_BYTES: usize = 64 * 1024 * 1024;
const MAX_TOTAL_BYTECODE_BYTES: usize = 256 * 1024 * 1024;
const MAX_CHUNKS: usize = 1_000_000;
const MAX_CHUNK_DEPTH: usize = 256;
const MAX_TYPE_DEPTH: usize = 256;
const MAX_VECTOR_ITEMS: usize = 8_000_000;
const MAX_FUNCTION_ARITY: usize = 65_535;

/// Errors produced while encoding or decoding a prelinked graph.
#[derive(Debug, Error)]
pub enum PrelinkedGraphCodecError {
  /// The input does not begin with the graph bundle signature.
  #[error("invalid prelinked graph bundle header")]
  InvalidHeader,
  /// The wire envelope version is not supported.
  #[error("unsupported prelinked graph wire version {0}")]
  UnsupportedVersion(u16),
  /// The supplied bytes or encoded result exceeds the codec limit.
  #[error("prelinked graph bundle is {actual} bytes; maximum supported size is {maximum} bytes")]
  TooLarge {
    /// Observed byte count.
    actual: usize,
    /// Maximum accepted byte count.
    maximum: usize,
  },
  /// A bounded wire field or aggregate resource exceeds its limit.
  #[error("prelinked graph {kind} is {actual}; maximum supported value is {maximum}")]
  LimitExceeded {
    /// Limited resource.
    kind: &'static str,
    /// Observed amount.
    actual: usize,
    /// Maximum accepted amount.
    maximum: usize,
  },
  /// The envelope length does not match the supplied input.
  #[error("prelinked graph bundle payload length does not match its header")]
  LengthMismatch,
  /// The payload checksum indicates corruption.
  #[error("prelinked graph bundle checksum mismatch")]
  ChecksumMismatch,
  /// A field cannot be represented or is malformed on the wire.
  #[error("invalid prelinked graph bundle data: {0}")]
  InvalidData(String),
  /// A decoded graph failed host-neutral graph or bytecode validation.
  #[error(transparent)]
  Graph(#[from] PrelinkedGraphError),
}

/// Encode a validated prelinked module graph into a deterministic wire bundle.
///
/// # Errors
///
/// Returns an error when graph validation fails or a wire-format limit is exceeded.
pub fn encode(graph: &PrelinkedModuleGraph) -> Result<Vec<u8>, PrelinkedGraphCodecError> {
  graph.validate()?;
  let mut writer = Writer::default();
  writer.u16(graph.format_version);
  writer.u8(graph.engine_bytecode_version as u8);
  writer.string(&graph.package_name, StringKind::Name)?;
  writer.string(&graph.package_version, StringKind::Name)?;
  writer.bytes(&graph.source_manifest_hash);
  writer.vector_len(graph.modules.len(), MAX_MODULES, "module count")?;
  let mut modules = graph.modules.iter().collect::<Vec<_>>();
  modules.sort_unstable_by_key(|module| module.module_index);
  for module in modules {
    encode_module(&mut writer, module)?;
  }
  writer.u32(graph.binding_count);
  writer.vector_len(
    graph.public_roots.len(),
    MAX_PUBLIC_ROOTS,
    "public root count",
  )?;
  let mut public_roots = graph.public_roots.clone();
  public_roots.sort_unstable();
  for root in public_roots {
    writer.u32(root);
  }
  frame_payload(&writer.bytes_out)
}

/// Decode, checksum, and strictly validate a prelinked module graph bundle.
///
/// # Errors
///
/// Returns an error for malformed, corrupt, unsupported, oversized, or invalid graph data.
pub fn decode(bytes: &[u8]) -> Result<PrelinkedModuleGraph, PrelinkedGraphCodecError> {
  if bytes.len() > MAX_WIRE_BYTES {
    return Err(PrelinkedGraphCodecError::TooLarge {
      actual: bytes.len(),
      maximum: MAX_WIRE_BYTES,
    });
  }
  if bytes.len() < HEADER_LEN || &bytes[..MAGIC.len()] != MAGIC {
    return Err(PrelinkedGraphCodecError::InvalidHeader);
  }
  let version = u16::from_le_bytes([bytes[8], bytes[9]]);
  if !(SOURCE_TEXT_WIRE_VERSION..=PRELINKED_GRAPH_WIRE_VERSION).contains(&version) {
    return Err(PrelinkedGraphCodecError::UnsupportedVersion(version));
  }
  let payload_len = u64::from_le_bytes(
    bytes[10..18]
      .try_into()
      .map_err(|_| PrelinkedGraphCodecError::InvalidHeader)?,
  );
  let payload_len =
    usize::try_from(payload_len).map_err(|_| invalid("payload length does not fit usize"))?;
  if payload_len > MAX_WIRE_BYTES - HEADER_LEN
    || bytes.len().checked_sub(HEADER_LEN) != Some(payload_len)
  {
    return Err(PrelinkedGraphCodecError::LengthMismatch);
  }
  let expected_checksum = u32::from_le_bytes(
    bytes[18..22]
      .try_into()
      .map_err(|_| PrelinkedGraphCodecError::InvalidHeader)?,
  );
  let payload = &bytes[HEADER_LEN..];
  if crc32fast::hash(payload) != expected_checksum {
    return Err(PrelinkedGraphCodecError::ChecksumMismatch);
  }

  let mut reader = Reader::new(payload, version);
  let graph = decode_graph(&mut reader)?;
  reader.finish()?;
  graph.validate()?;
  Ok(graph)
}

fn encode_module(
  writer: &mut Writer,
  module: &PrelinkedModule,
) -> Result<(), PrelinkedGraphCodecError> {
  writer.string(&module.specifier, StringKind::Name)?;
  writer.u32(module.module_index);
  writer.bytes(&module.source_sha256);
  writer.vector_len(
    module.dependencies.len(),
    MAX_DEPENDENCIES,
    "dependency count",
  )?;
  for dependency in &module.dependencies {
    writer.u32(*dependency);
  }
  writer.vector_len(module.exports.len(), MAX_BINDING_ENTRIES, "export count")?;
  let mut exports = module.exports.iter().collect::<Vec<_>>();
  exports.sort_unstable_by(|left, right| left.exported_name.cmp(&right.exported_name));
  for export in exports {
    writer.string(&export.exported_name, StringKind::Name)?;
    writer.u32(export.binding_id);
  }
  writer.vector_len(module.imports.len(), MAX_BINDING_ENTRIES, "import count")?;
  let mut imports = module.imports.iter().collect::<Vec<_>>();
  imports.sort_unstable_by(|left, right| left.local_name.cmp(&right.local_name));
  for import in imports {
    writer.string(&import.local_name, StringKind::Name)?;
    writer.u32(import.binding_id);
  }
  writer.string(&module.chunk.source_text, StringKind::Source)?;
  encode_chunk(writer, &module.chunk, &module.chunk.source_text, 0)
}

fn decode_graph(reader: &mut Reader<'_>) -> Result<PrelinkedModuleGraph, PrelinkedGraphCodecError> {
  let format_version = reader.u16()?;
  let engine_bytecode_version = decode_bytecode_version(reader.u8()?)?;
  let package_name = reader.string(StringKind::Name)?;
  let package_version = reader.string(StringKind::Name)?;
  let source_manifest_hash = reader.array_32()?;
  let module_count = reader.vector_len(MAX_MODULES, "module count")?;
  let mut modules = Vec::with_capacity(module_count);
  for _ in 0..module_count {
    modules.push(decode_module(reader)?);
  }
  let binding_count = reader.u32()?;
  if usize::try_from(binding_count)
    .ok()
    .is_none_or(|count| count > MAX_BINDINGS)
  {
    return Err(limit("binding count", binding_count as usize, MAX_BINDINGS));
  }
  let root_count = reader.vector_len(MAX_PUBLIC_ROOTS, "public root count")?;
  let mut public_roots = Vec::with_capacity(root_count);
  for _ in 0..root_count {
    public_roots.push(reader.u32()?);
  }
  Ok(PrelinkedModuleGraph {
    format_version,
    engine_bytecode_version,
    package_name,
    package_version,
    source_manifest_hash,
    modules,
    binding_count,
    public_roots,
  })
}

fn decode_module(reader: &mut Reader<'_>) -> Result<PrelinkedModule, PrelinkedGraphCodecError> {
  let specifier = reader.string(StringKind::Name)?;
  let module_index = reader.u32()?;
  let source_sha256 = reader.array_32()?;
  let dependency_count = reader.vector_len(MAX_DEPENDENCIES, "dependency count")?;
  reader.add_dependency_budget(dependency_count)?;
  let mut dependencies = Vec::with_capacity(dependency_count);
  for _ in 0..dependency_count {
    dependencies.push(reader.u32()?);
  }
  let export_count = reader.vector_len(MAX_BINDING_ENTRIES, "export count")?;
  reader.add_binding_budget(export_count)?;
  let mut exports = Vec::with_capacity(export_count);
  for _ in 0..export_count {
    exports.push(PrelinkedExport {
      exported_name: reader.string(StringKind::Name)?,
      binding_id: reader.u32()?,
    });
  }
  let import_count = reader.vector_len(MAX_BINDING_ENTRIES, "import count")?;
  reader.add_binding_budget(import_count)?;
  let mut imports = Vec::with_capacity(import_count);
  for _ in 0..import_count {
    imports.push(PrelinkedImport {
      local_name: reader.string(StringKind::Name)?,
      binding_id: reader.u32()?,
    });
  }
  let module_source = if reader.wire_version >= 3 {
    Some(Arc::new(reader.string(StringKind::Source)?))
  } else {
    None
  };
  let code = decode_chunk(reader, 0, module_source.as_ref())?;
  Ok(PrelinkedModule {
    specifier,
    module_index,
    source_sha256,
    dependencies,
    exports,
    imports,
    chunk: code,
  })
}

fn encode_chunk(
  writer: &mut Writer,
  code: &Chunk,
  module_source: &str,
  depth: usize,
) -> Result<(), PrelinkedGraphCodecError> {
  if depth > MAX_CHUNK_DEPTH {
    return Err(invalid("code nesting exceeds the supported depth"));
  }
  if code.source_text.as_str() != module_source {
    return Err(invalid("all chunks in a module must share source text"));
  }
  writer.u8(code.version as u8);
  writer.vector_len(code.words.len(), MAX_VECTOR_ITEMS, "instruction count")?;
  for word in &code.words {
    writer.u32(*word);
  }
  writer.vector_len(code.constants.len(), MAX_VECTOR_ITEMS, "constant count")?;
  for constant in &code.constants {
    encode_constant(writer, constant)?;
  }
  writer.vector_len(code.functions.len(), MAX_VECTOR_ITEMS, "function count")?;
  for function in &code.functions {
    encode_function(writer, function, module_source, depth + 1)?;
  }
  writer.vector_len(code.lines.len(), MAX_VECTOR_ITEMS, "line count")?;
  for line in &code.lines {
    writer.u32(*line);
  }
  encode_optional_types(writer, &code.root_slot_types, 0)?;
  encode_optional_strings(writer, &code.root_slot_names)?;
  encode_bools(writer, &code.root_lexical_tdz)?;
  encode_bools(writer, &code.root_slot_immutable)?;
  writer.u16(code.spill_slot_count);
  writer.u16(code.value_spill_slot_count);
  writer.bool(code.root_strict);
  writer.bool(code.has_top_level_await);
  writer.bool(code.allow_new_target);
  writer.bool(code.allow_super);
  writer.vector_len(
    code.finally_handlers.len(),
    MAX_VECTOR_ITEMS,
    "finally handler count",
  )?;
  for handler in &code.finally_handlers {
    writer.usize(handler.push_pc, "finally push instruction")?;
    writer.usize(handler.finalizer_pc, "finalizer instruction")?;
    writer.usize(handler.return_slot, "finally return slot")?;
    writer.usize(handler.did_return_slot, "finally did-return slot")?;
  }
  Ok(())
}

fn decode_chunk(
  reader: &mut Reader<'_>,
  depth: usize,
  module_source: Option<&Arc<String>>,
) -> Result<Chunk, PrelinkedGraphCodecError> {
  if depth > MAX_CHUNK_DEPTH {
    return Err(invalid("code nesting exceeds the supported depth"));
  }
  reader.chunks = reader
    .chunks
    .checked_add(1)
    .ok_or_else(|| invalid("code count overflow"))?;
  if reader.chunks > MAX_CHUNKS {
    return Err(limit("code count", reader.chunks, MAX_CHUNKS));
  }
  let version = decode_bytecode_version(reader.u8()?)?;
  let code_count = reader.vector_len(MAX_VECTOR_ITEMS, "instruction count")?;
  reader.add_bytecode_budget(
    code_count
      .checked_mul(4)
      .ok_or_else(|| invalid("instruction size overflow"))?,
  )?;
  let mut code = Vec::with_capacity(code_count);
  for _ in 0..code_count {
    code.push(reader.u32()?);
  }
  let constant_count = reader.vector_len(MAX_VECTOR_ITEMS, "constant count")?;
  let mut constants = Vec::with_capacity(constant_count);
  for _ in 0..constant_count {
    constants.push(decode_constant(reader)?);
  }
  let function_count = reader.vector_len(MAX_VECTOR_ITEMS, "function count")?;
  let mut functions = Vec::with_capacity(function_count);
  for _ in 0..function_count {
    functions.push(decode_function(reader, depth + 1, module_source)?);
  }
  let line_count = reader.vector_len(MAX_VECTOR_ITEMS, "line count")?;
  reader.add_bytecode_budget(
    line_count
      .checked_mul(4)
      .ok_or_else(|| invalid("line table size overflow"))?,
  )?;
  let mut lines = Vec::with_capacity(line_count);
  for _ in 0..line_count {
    lines.push(reader.u32()?);
  }
  let source_text = if reader.wire_version == SOURCE_TEXT_WIRE_VERSION {
    Arc::new(reader.string(StringKind::Source)?)
  } else {
    Arc::clone(module_source.ok_or_else(|| invalid("module source text is missing"))?)
  };
  let root_slot_types = decode_optional_types(reader, 0)?;
  let root_slot_names = decode_optional_strings(reader)?;
  let root_lexical_tdz = decode_bools(reader)?;
  let root_slot_immutable = decode_bools(reader)?;
  let spill_slot_count = reader.u16()?;
  let value_spill_slot_count = reader.u16()?;
  let root_strict = reader.bool()?;
  let has_top_level_await = reader.wire_version >= 6 && reader.bool()?;
  let allow_new_target = reader.bool()?;
  let allow_super = reader.bool()?;
  let handler_count = reader.vector_len(MAX_VECTOR_ITEMS, "finally handler count")?;
  let mut finally_handlers = Vec::with_capacity(handler_count);
  for _ in 0..handler_count {
    finally_handlers.push(FinallyHandler {
      push_pc: reader.portable_usize("finally push instruction")?,
      finalizer_pc: reader.portable_usize("finalizer instruction")?,
      return_slot: reader.portable_usize("finally return slot")?,
      did_return_slot: reader.portable_usize("finally did-return slot")?,
    });
  }
  Ok(Chunk {
    version,
    words: code,
    constants,
    functions,
    lines,
    source_text,
    root_slot_types,
    root_slot_names,
    root_lexical_tdz,
    root_slot_immutable,
    root_function_declarations: Vec::new(),
    spill_slot_count,
    value_spill_slot_count,
    root_strict,
    has_top_level_await,
    allow_new_target,
    allow_super,
    finally_handlers,
  })
}

fn encode_function(
  writer: &mut Writer,
  function: &Function,
  module_source: &str,
  depth: usize,
) -> Result<(), PrelinkedGraphCodecError> {
  writer.string(&function.name, StringKind::Name)?;
  writer.u32(function.span.start);
  writer.u32(function.span.end);
  if function.arity > MAX_FUNCTION_ARITY {
    return Err(limit("function arity", function.arity, MAX_FUNCTION_ARITY));
  }
  writer.u64(
    u64::try_from(function.arity)
      .map_err(|_| invalid("function arity does not fit portable u64"))?,
  );
  writer.u64(
    u64::try_from(function.length)
      .map_err(|_| invalid("function length does not fit portable u64"))?,
  );
  encode_chunk(writer, &function.chunk, module_source, depth)?;
  encode_optional_types(writer, &function.slot_types, 0)?;
  encode_optional_strings(writer, &function.slot_names)?;
  encode_bools(writer, &function.lexical_tdz)?;
  encode_bools(writer, &function.slot_immutable)?;
  encode_bools(writer, &function.slot_sloppy_immutable_silent)?;
  writer.vector_len(function.upvalues.len(), MAX_VECTOR_ITEMS, "upvalue count")?;
  for upvalue in &function.upvalues {
    writer.bool(upvalue.is_local);
    writer.u16(upvalue.index);
    writer.option_string(upvalue.mirror_global_name.as_deref(), StringKind::Name)?;
    writer.bool(upvalue.immutable);
    writer.bool(upvalue.sloppy_immutable_silent);
  }
  writer.bool(function.has_rest);
  writer.bool(function.has_prototype_property);
  writer.bool(function.captures_this);
  writer.option_usize(function.arguments_slot, "arguments slot")?;
  writer.option_usize(function.self_binding_slot, "self-binding slot")?;
  writer.bool(function.has_simple_parameter_list);
  encode_optional_type(writer, function.return_type.as_ref(), 0)?;
  writer.bool(function.is_strict);
  writer.bool(function.is_constructor);
  writer.bool(function.is_derived_constructor);
  writer.bool(function.is_class_constructor);
  writer.bool(function.is_async);
  writer.bool(function.is_generator);
  writer.vector_len(0, MAX_VECTOR_ITEMS, "inline cache count")?;
  Ok(())
}

fn decode_function(
  reader: &mut Reader<'_>,
  depth: usize,
  module_source: Option<&Arc<String>>,
) -> Result<Function, PrelinkedGraphCodecError> {
  let name = reader.string(StringKind::Name)?;
  let span_start = reader.u32()?;
  let span_end = reader.u32()?;
  let arity = reader.portable_usize("function arity")?;
  if arity > MAX_FUNCTION_ARITY {
    return Err(limit("function arity", arity, MAX_FUNCTION_ARITY));
  }
  let length = if reader.wire_version >= 4 {
    reader.portable_usize("function length")?
  } else {
    arity
  };
  if length > MAX_FUNCTION_ARITY {
    return Err(limit("function length", length, MAX_FUNCTION_ARITY));
  }
  let code = Arc::new(decode_chunk(reader, depth, module_source)?);
  let slot_types = decode_optional_types(reader, 0)?;
  let slot_names = decode_optional_strings(reader)?;
  let lexical_tdz = decode_bools(reader)?;
  let slot_immutable = decode_bools(reader)?;
  let slot_sloppy_immutable_silent = decode_bools(reader)?;
  let upvalue_count = reader.vector_len(MAX_VECTOR_ITEMS, "upvalue count")?;
  let mut upvalues = Vec::with_capacity(upvalue_count);
  for _ in 0..upvalue_count {
    let is_local = reader.bool()?;
    let index = if reader.wire_version >= 5 {
      reader.u16()?
    } else {
      u16::from(reader.u8()?)
    };
    upvalues.push(UpvalueSpec {
      is_local,
      index,
      mirror_global_name: reader.option_string(StringKind::Name)?,
      immutable: reader.bool()?,
      sloppy_immutable_silent: reader.bool()?,
    });
  }
  let has_rest = reader.bool()?;
  let has_prototype_property = reader.bool()?;
  let captures_this = reader.bool()?;
  let arguments_slot = reader.option_usize("arguments slot")?;
  let self_binding_slot = reader.option_usize("self-binding slot")?;
  let has_simple_parameter_list = reader.bool()?;
  let return_type = decode_optional_type(reader, 0)?;
  let is_strict = reader.bool()?;
  let is_constructor = reader.bool()?;
  let is_derived_constructor = reader.bool()?;
  let is_class_constructor = reader.bool()?;
  let is_async = reader.bool()?;
  let is_generator = reader.bool()?;
  let ic_count = reader.vector_len(MAX_VECTOR_ITEMS, "inline cache count")?;
  for _ in 0..ic_count {
    let _ = reader.u64()?;
    let _ = reader.u64()?;
    let _ = reader.u32()?;
    let _ = reader.u8()?;
    let _ = reader.u8()?;
  }
  Ok(Function {
    name,
    span: Span::new(span_start, span_end),
    arity,
    length,
    chunk: code,
    slot_types,
    slot_names,
    lexical_tdz,
    slot_immutable,
    slot_sloppy_immutable_silent,
    upvalues,
    has_rest,
    has_prototype_property,
    captures_this,
    arguments_slot,
    self_binding_slot,
    has_simple_parameter_list,
    return_type,
    is_strict,
    is_constructor,
    is_derived_constructor,
    is_class_constructor,
    is_async,
    is_generator,
  })
}

fn encode_constant(
  writer: &mut Writer,
  constant: &Constant,
) -> Result<(), PrelinkedGraphCodecError> {
  match constant {
    Constant::Uninitialized => writer.u8(0),
    Constant::Number(value) => {
      writer.u8(1);
      writer.u64(value.to_bits());
    }
    Constant::String(value) => {
      writer.u8(2);
      writer.js_string(value)?;
    }
    Constant::Bool(value) => {
      writer.u8(3);
      writer.bool(*value);
    }
    Constant::Undefined => writer.u8(4),
    Constant::BigInt(value) => {
      writer.u8(5);
      let (sign, bytes) = value.to_bytes_le();
      writer.u8(encode_sign(sign));
      writer.bytes_vec(&bytes, MAX_TOTAL_BYTECODE_BYTES, "bigint byte count")?;
    }
    Constant::Symbol(id, description) => {
      writer.u8(6);
      writer.u64(id.as_u64());
      writer.option_js_string(description.as_ref())?;
    }
    Constant::Null => writer.u8(7),
  }
  Ok(())
}

fn decode_constant(reader: &mut Reader<'_>) -> Result<Constant, PrelinkedGraphCodecError> {
  Ok(match reader.u8()? {
    0 => Constant::Uninitialized,
    1 => Constant::Number(f64::from_bits(reader.u64()?)),
    2 => Constant::String(reader.js_string()?),
    3 => Constant::Bool(reader.bool()?),
    4 => Constant::Undefined,
    5 => {
      let sign = decode_sign(reader.u8()?)?;
      let bytes = reader.bytes_vec(MAX_TOTAL_BYTECODE_BYTES, "bigint byte count")?;
      if (sign == Sign::NoSign && !bytes.is_empty())
        || (sign != Sign::NoSign && bytes.last().is_none_or(|last| *last == 0))
      {
        return Err(invalid("non-canonical bigint encoding"));
      }
      Constant::BigInt(BigInt::from_bytes_le(sign, &bytes))
    }
    6 => {
      let id = reader.u64()?;
      Constant::Symbol(SymbolId::well_known(id), reader.option_js_string()?)
    }
    7 => Constant::Null,
    tag => return Err(invalid(format!("unknown constant tag {tag}"))),
  })
}

fn encode_optional_types(
  writer: &mut Writer,
  values: &[Option<Type>],
  depth: usize,
) -> Result<(), PrelinkedGraphCodecError> {
  writer.vector_len(values.len(), MAX_VECTOR_ITEMS, "type entry count")?;
  for value in values {
    encode_optional_type(writer, value.as_ref(), depth)?;
  }
  Ok(())
}

fn encode_optional_type(
  writer: &mut Writer,
  value: Option<&Type>,
  depth: usize,
) -> Result<(), PrelinkedGraphCodecError> {
  match value {
    None => writer.u8(0),
    Some(value) => {
      writer.u8(1);
      encode_type(writer, value, depth)?;
    }
  }
  Ok(())
}

fn decode_optional_types(
  reader: &mut Reader<'_>,
  depth: usize,
) -> Result<Vec<Option<Type>>, PrelinkedGraphCodecError> {
  let count = reader.vector_len(MAX_VECTOR_ITEMS, "type entry count")?;
  let mut values = Vec::with_capacity(count);
  for _ in 0..count {
    values.push(decode_optional_type(reader, depth)?);
  }
  Ok(values)
}

fn decode_optional_type(
  reader: &mut Reader<'_>,
  depth: usize,
) -> Result<Option<Type>, PrelinkedGraphCodecError> {
  match reader.u8()? {
    0 => Ok(None),
    1 => Ok(Some(decode_type(reader, depth)?)),
    tag => Err(invalid(format!("invalid optional type tag {tag}"))),
  }
}

fn encode_type(
  writer: &mut Writer,
  value: &Type,
  depth: usize,
) -> Result<(), PrelinkedGraphCodecError> {
  if depth > MAX_TYPE_DEPTH {
    return Err(invalid("type nesting exceeds the supported depth"));
  }
  match value {
    Type::Never => writer.u8(0),
    Type::Number => writer.u8(1),
    Type::String => writer.u8(2),
    Type::Boolean => writer.u8(3),
    Type::Null => writer.u8(4),
    Type::Undefined => writer.u8(5),
    Type::Void => writer.u8(6),
    Type::Any => writer.u8(7),
    Type::Unknown => writer.u8(8),
    Type::Array(value) => {
      writer.u8(9);
      encode_type(writer, value, depth + 1)?;
    }
    Type::Tuple(values) => {
      writer.u8(10);
      encode_type_vec(writer, values, depth)?;
    }
    Type::Rest(value) => {
      writer.u8(11);
      encode_type(writer, value, depth + 1)?;
    }
    Type::Function(parameters, result) => {
      writer.u8(12);
      encode_type_vec(writer, parameters, depth)?;
      encode_type(writer, result, depth + 1)?;
    }
    Type::Object(properties) => {
      writer.u8(13);
      let mut properties = properties.iter().collect::<Vec<_>>();
      properties.sort_by(|left, right| left.0.cmp(right.0));
      writer.vector_len(properties.len(), MAX_VECTOR_ITEMS, "object property count")?;
      for (name, value) in properties {
        writer.string(name, StringKind::Name)?;
        encode_type(writer, value, depth + 1)?;
      }
    }
    Type::Builtin(value) => {
      writer.u8(14);
      writer.u8(encode_builtin(value));
    }
    Type::TypeReference { name, arguments } => {
      writer.u8(15);
      writer.string(name, StringKind::Name)?;
      encode_type_vec(writer, arguments, depth)?;
    }
    Type::Union(values) => {
      writer.u8(16);
      encode_type_vec(writer, values, depth)?;
    }
    Type::Intersection(values) => {
      writer.u8(17);
      encode_type_vec(writer, values, depth)?;
    }
    Type::Literal(value) => {
      writer.u8(18);
      encode_literal(writer, value)?;
    }
    Type::Conditional {
      check,
      extends,
      true_type,
      false_type,
      distributive_parameter,
    } => {
      writer.u8(19);
      encode_type(writer, check, depth + 1)?;
      encode_type(writer, extends, depth + 1)?;
      encode_type(writer, true_type, depth + 1)?;
      encode_type(writer, false_type, depth + 1)?;
      writer.option_string(distributive_parameter.as_deref(), StringKind::Name)?;
    }
    Type::KeyOf(value) => {
      writer.u8(20);
      encode_type(writer, value, depth + 1)?;
    }
    Type::IndexedAccess { object, index } => {
      writer.u8(21);
      encode_type(writer, object, depth + 1)?;
      encode_type(writer, index, depth + 1)?;
    }
    Type::Infer(value) => {
      writer.u8(22);
      writer.string(value, StringKind::Name)?;
    }
    Type::Mapped {
      parameter,
      constraint,
      value,
    } => {
      writer.u8(23);
      writer.string(parameter, StringKind::Name)?;
      encode_type(writer, constraint, depth + 1)?;
      encode_type(writer, value, depth + 1)?;
    }
    Type::TemplateLiteral {
      quasis,
      substitutions,
    } => {
      writer.u8(24);
      writer.vector_len(quasis.len(), MAX_VECTOR_ITEMS, "template quasi count")?;
      for quasi in quasis {
        writer.string(quasi, StringKind::Name)?;
      }
      encode_type_vec(writer, substitutions, depth)?;
    }
  }
  Ok(())
}

fn encode_type_vec(
  writer: &mut Writer,
  values: &[Type],
  depth: usize,
) -> Result<(), PrelinkedGraphCodecError> {
  writer.vector_len(values.len(), MAX_VECTOR_ITEMS, "type count")?;
  for value in values {
    encode_type(writer, value, depth + 1)?;
  }
  Ok(())
}

fn decode_type(
  reader: &mut Reader<'_>,
  depth: usize,
) -> Result<Type, PrelinkedGraphCodecError> {
  if depth > MAX_TYPE_DEPTH {
    return Err(invalid("type nesting exceeds the supported depth"));
  }
  Ok(match reader.u8()? {
    0 => Type::Never,
    1 => Type::Number,
    2 => Type::String,
    3 => Type::Boolean,
    4 => Type::Null,
    5 => Type::Undefined,
    6 => Type::Void,
    7 => Type::Any,
    8 => Type::Unknown,
    9 => Type::Array(Box::new(decode_type(reader, depth + 1)?)),
    10 => Type::Tuple(decode_type_vec(reader, depth)?),
    11 => Type::Rest(Box::new(decode_type(reader, depth + 1)?)),
    12 => {
      let parameters = decode_type_vec(reader, depth)?;
      let result = Box::new(decode_type(reader, depth + 1)?);
      Type::Function(parameters, result)
    }
    13 => {
      let count = reader.vector_len(MAX_VECTOR_ITEMS, "object property count")?;
      let mut properties = HashMap::with_capacity(count);
      let mut previous_name = None;
      for _ in 0..count {
        let name = reader.string(StringKind::Name)?;
        if previous_name
          .as_ref()
          .is_some_and(|previous| previous >= &name)
        {
          return Err(invalid(
            "object type properties are not in canonical order or are duplicated",
          ));
        }
        previous_name = Some(name.clone());
        let value = decode_type(reader, depth + 1)?;
        if properties.insert(name, value).is_some() {
          return Err(invalid("object type contains duplicate property names"));
        }
      }
      Type::Object(properties)
    }
    14 => Type::Builtin(decode_builtin(reader.u8()?)?),
    15 => Type::TypeReference {
      name: reader.string(StringKind::Name)?,
      arguments: decode_type_vec(reader, depth)?,
    },
    16 => Type::Union(decode_type_vec(reader, depth)?),
    17 => Type::Intersection(decode_type_vec(reader, depth)?),
    18 => Type::Literal(decode_literal(reader)?),
    19 => Type::Conditional {
      check: Box::new(decode_type(reader, depth + 1)?),
      extends: Box::new(decode_type(reader, depth + 1)?),
      true_type: Box::new(decode_type(reader, depth + 1)?),
      false_type: Box::new(decode_type(reader, depth + 1)?),
      distributive_parameter: reader.option_string(StringKind::Name)?,
    },
    20 => Type::KeyOf(Box::new(decode_type(reader, depth + 1)?)),
    21 => Type::IndexedAccess {
      object: Box::new(decode_type(reader, depth + 1)?),
      index: Box::new(decode_type(reader, depth + 1)?),
    },
    22 => Type::Infer(reader.string(StringKind::Name)?),
    23 => Type::Mapped {
      parameter: reader.string(StringKind::Name)?,
      constraint: Box::new(decode_type(reader, depth + 1)?),
      value: Box::new(decode_type(reader, depth + 1)?),
    },
    24 => {
      let quasi_count = reader.vector_len(MAX_VECTOR_ITEMS, "template quasi count")?;
      let mut quasis = Vec::with_capacity(quasi_count);
      for _ in 0..quasi_count {
        quasis.push(reader.string(StringKind::Name)?);
      }
      Type::TemplateLiteral {
        quasis,
        substitutions: decode_type_vec(reader, depth)?,
      }
    }
    tag => return Err(invalid(format!("unknown type tag {tag}"))),
  })
}

fn decode_type_vec(
  reader: &mut Reader<'_>,
  depth: usize,
) -> Result<Vec<Type>, PrelinkedGraphCodecError> {
  let count = reader.vector_len(MAX_VECTOR_ITEMS, "type count")?;
  let mut values = Vec::with_capacity(count);
  for _ in 0..count {
    values.push(decode_type(reader, depth + 1)?);
  }
  Ok(values)
}

fn encode_optional_strings(
  writer: &mut Writer,
  values: &[Option<String>],
) -> Result<(), PrelinkedGraphCodecError> {
  writer.vector_len(values.len(), MAX_VECTOR_ITEMS, "optional string count")?;
  for value in values {
    writer.option_string(value.as_deref(), StringKind::Name)?;
  }
  Ok(())
}

fn decode_optional_strings(
  reader: &mut Reader<'_>
) -> Result<Vec<Option<String>>, PrelinkedGraphCodecError> {
  let count = reader.vector_len(MAX_VECTOR_ITEMS, "optional string count")?;
  let mut values = Vec::with_capacity(count);
  for _ in 0..count {
    values.push(reader.option_string(StringKind::Name)?);
  }
  Ok(values)
}

fn encode_bools(
  writer: &mut Writer,
  values: &[bool],
) -> Result<(), PrelinkedGraphCodecError> {
  writer.vector_len(values.len(), MAX_VECTOR_ITEMS, "boolean count")?;
  for value in values {
    writer.bool(*value);
  }
  Ok(())
}

fn decode_bools(reader: &mut Reader<'_>) -> Result<Vec<bool>, PrelinkedGraphCodecError> {
  let count = reader.vector_len(MAX_VECTOR_ITEMS, "boolean count")?;
  let mut values = Vec::with_capacity(count);
  for _ in 0..count {
    values.push(reader.bool()?);
  }
  Ok(values)
}

fn encode_literal(
  writer: &mut Writer,
  value: &LiteralType,
) -> Result<(), PrelinkedGraphCodecError> {
  match value {
    LiteralType::String(value) => {
      writer.u8(0);
      writer.string(value, StringKind::Name)?;
    }
    LiteralType::Number(value) => {
      writer.u8(1);
      writer.u64(*value);
    }
    LiteralType::Boolean(value) => {
      writer.u8(2);
      writer.bool(*value);
    }
  }
  Ok(())
}

fn decode_literal(reader: &mut Reader<'_>) -> Result<LiteralType, PrelinkedGraphCodecError> {
  Ok(match reader.u8()? {
    0 => LiteralType::String(reader.string(StringKind::Name)?),
    1 => LiteralType::Number(reader.u64()?),
    2 => LiteralType::Boolean(reader.bool()?),
    tag => return Err(invalid(format!("unknown literal type tag {tag}"))),
  })
}

fn encode_builtin(value: &BuiltinType) -> u8 {
  match value {
    BuiltinType::Undefined => 0,
    BuiltinType::Null => 1,
    BuiltinType::Boolean => 2,
    BuiltinType::Number => 3,
    BuiltinType::String => 4,
    BuiltinType::Symbol => 5,
    BuiltinType::BigInt => 6,
    BuiltinType::Object => 7,
    BuiltinType::Function => 8,
    BuiltinType::Array => 9,
  }
}

fn decode_builtin(tag: u8) -> Result<BuiltinType, PrelinkedGraphCodecError> {
  Ok(match tag {
    0 => BuiltinType::Undefined,
    1 => BuiltinType::Null,
    2 => BuiltinType::Boolean,
    3 => BuiltinType::Number,
    4 => BuiltinType::String,
    5 => BuiltinType::Symbol,
    6 => BuiltinType::BigInt,
    7 => BuiltinType::Object,
    8 => BuiltinType::Function,
    9 => BuiltinType::Array,
    tag => return Err(invalid(format!("unknown builtin type tag {tag}"))),
  })
}

fn encode_sign(value: Sign) -> u8 {
  match value {
    Sign::NoSign => 0,
    Sign::Plus => 1,
    Sign::Minus => 2,
  }
}

fn decode_sign(tag: u8) -> Result<Sign, PrelinkedGraphCodecError> {
  match tag {
    0 => Ok(Sign::NoSign),
    1 => Ok(Sign::Plus),
    2 => Ok(Sign::Minus),
    tag => Err(invalid(format!("unknown bigint sign tag {tag}"))),
  }
}

fn decode_bytecode_version(tag: u8) -> Result<BytecodeVersion, PrelinkedGraphCodecError> {
  match tag {
    value if value == BytecodeVersion::RiscV2 as u8 => Ok(BytecodeVersion::RiscV2),
    value => Err(invalid(format!("unsupported bytecode version {value}"))),
  }
}

fn frame_payload(payload: &[u8]) -> Result<Vec<u8>, PrelinkedGraphCodecError> {
  let total = HEADER_LEN
    .checked_add(payload.len())
    .ok_or_else(|| invalid("bundle length overflow"))?;
  if total > MAX_WIRE_BYTES {
    return Err(PrelinkedGraphCodecError::TooLarge {
      actual: total,
      maximum: MAX_WIRE_BYTES,
    });
  }
  let payload_len = u64::try_from(payload.len()).map_err(|_| invalid("payload length overflow"))?;
  let mut bytes = Vec::with_capacity(total);
  bytes.extend_from_slice(MAGIC);
  bytes.extend_from_slice(&PRELINKED_GRAPH_WIRE_VERSION.to_le_bytes());
  bytes.extend_from_slice(&payload_len.to_le_bytes());
  bytes.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
  bytes.extend_from_slice(payload);
  Ok(bytes)
}

fn invalid(message: impl Into<String>) -> PrelinkedGraphCodecError {
  PrelinkedGraphCodecError::InvalidData(message.into())
}

fn limit(
  kind: &'static str,
  actual: usize,
  maximum: usize,
) -> PrelinkedGraphCodecError {
  PrelinkedGraphCodecError::LimitExceeded {
    kind,
    actual,
    maximum,
  }
}

#[derive(Default)]
struct Writer {
  bytes_out: Vec<u8>,
  name_bytes: usize,
  source_bytes: usize,
}

#[derive(Clone, Copy)]
enum StringKind {
  Name,
  Source,
}

impl Writer {
  fn u8(
    &mut self,
    value: u8,
  ) {
    self.bytes_out.push(value);
  }

  fn bool(
    &mut self,
    value: bool,
  ) {
    self.u8(u8::from(value));
  }

  fn u16(
    &mut self,
    value: u16,
  ) {
    self
      .bytes_out
      .extend_from_slice(&value.to_le_bytes());
  }

  fn u32(
    &mut self,
    value: u32,
  ) {
    self
      .bytes_out
      .extend_from_slice(&value.to_le_bytes());
  }

  fn u64(
    &mut self,
    value: u64,
  ) {
    self
      .bytes_out
      .extend_from_slice(&value.to_le_bytes());
  }

  fn bytes(
    &mut self,
    value: &[u8],
  ) {
    self.bytes_out.extend_from_slice(value);
  }

  fn vector_len(
    &mut self,
    length: usize,
    maximum: usize,
    kind: &'static str,
  ) -> Result<(), PrelinkedGraphCodecError> {
    if length > maximum {
      return Err(limit(kind, length, maximum));
    }
    let length = u32::try_from(length).map_err(|_| invalid(format!("{kind} does not fit u32")))?;
    self.u32(length);
    Ok(())
  }

  fn string(
    &mut self,
    value: &str,
    kind: StringKind,
  ) -> Result<(), PrelinkedGraphCodecError> {
    let maximum = match kind {
      StringKind::Name => MAX_NAME_BYTES,
      StringKind::Source => MAX_TOTAL_SOURCE_BYTES,
    };
    if value.len() > maximum {
      return Err(limit("string byte count", value.len(), maximum));
    }
    let total = match kind {
      StringKind::Name => &mut self.name_bytes,
      StringKind::Source => &mut self.source_bytes,
    };
    *total = total
      .checked_add(value.len())
      .ok_or_else(|| invalid("string byte count overflow"))?;
    let maximum_total = match kind {
      StringKind::Name => MAX_TOTAL_NAME_BYTES,
      StringKind::Source => MAX_TOTAL_SOURCE_BYTES,
    };
    if *total > maximum_total {
      return Err(limit("total string bytes", *total, maximum_total));
    }
    self.vector_len(value.len(), u32::MAX as usize, "string byte count")?;
    self.bytes(value.as_bytes());
    Ok(())
  }

  fn option_string(
    &mut self,
    value: Option<&str>,
    kind: StringKind,
  ) -> Result<(), PrelinkedGraphCodecError> {
    match value {
      None => self.u8(0),
      Some(value) => {
        self.u8(1);
        self.string(value, kind)?;
      }
    }
    Ok(())
  }

  fn js_string(
    &mut self,
    value: &js_string::JsString,
  ) -> Result<(), PrelinkedGraphCodecError> {
    self.vector_len(value.len(), MAX_NAME_BYTES / 2, "UTF-16 code unit count")?;
    for unit in value.code_units() {
      self.u16(unit);
    }
    Ok(())
  }

  fn option_js_string(
    &mut self,
    value: Option<&js_string::JsString>,
  ) -> Result<(), PrelinkedGraphCodecError> {
    match value {
      None => self.u8(0),
      Some(value) => {
        self.u8(1);
        self.js_string(value)?;
      }
    }
    Ok(())
  }

  fn bytes_vec(
    &mut self,
    value: &[u8],
    maximum: usize,
    kind: &'static str,
  ) -> Result<(), PrelinkedGraphCodecError> {
    self.vector_len(value.len(), maximum, kind)?;
    self.bytes(value);
    Ok(())
  }

  fn usize(
    &mut self,
    value: usize,
    kind: &'static str,
  ) -> Result<(), PrelinkedGraphCodecError> {
    self.u64(u64::try_from(value).map_err(|_| invalid(format!("{kind} does not fit u64")))?);
    Ok(())
  }

  fn option_usize(
    &mut self,
    value: Option<usize>,
    kind: &'static str,
  ) -> Result<(), PrelinkedGraphCodecError> {
    match value {
      None => self.u8(0),
      Some(value) => {
        self.u8(1);
        self.usize(value, kind)?;
      }
    }
    Ok(())
  }
}

struct Reader<'a> {
  bytes: &'a [u8],
  position: usize,
  wire_version: u16,
  name_bytes: usize,
  source_bytes: usize,
  bytecode_bytes: usize,
  dependency_count: usize,
  binding_count: usize,
  chunks: usize,
}

impl<'a> Reader<'a> {
  fn new(
    bytes: &'a [u8],
    wire_version: u16,
  ) -> Self {
    Self {
      bytes,
      position: 0,
      wire_version,
      name_bytes: 0,
      source_bytes: 0,
      bytecode_bytes: 0,
      dependency_count: 0,
      binding_count: 0,
      chunks: 0,
    }
  }

  fn remaining(&self) -> usize {
    self.bytes.len().saturating_sub(self.position)
  }

  fn take(
    &mut self,
    length: usize,
  ) -> Result<&'a [u8], PrelinkedGraphCodecError> {
    let end = self
      .position
      .checked_add(length)
      .ok_or_else(|| invalid("input position overflow"))?;
    if end > self.bytes.len() {
      return Err(invalid("unexpected end of prelinked graph payload"));
    }
    let result = &self.bytes[self.position..end];
    self.position = end;
    Ok(result)
  }

  fn u8(&mut self) -> Result<u8, PrelinkedGraphCodecError> {
    Ok(
      *self
        .take(1)?
        .first()
        .ok_or_else(|| invalid("missing byte"))?,
    )
  }

  fn bool(&mut self) -> Result<bool, PrelinkedGraphCodecError> {
    match self.u8()? {
      0 => Ok(false),
      1 => Ok(true),
      value => Err(invalid(format!("invalid boolean value {value}"))),
    }
  }

  fn u16(&mut self) -> Result<u16, PrelinkedGraphCodecError> {
    Ok(u16::from_le_bytes(
      self
        .take(2)?
        .try_into()
        .map_err(|_| invalid("missing u16"))?,
    ))
  }

  fn u32(&mut self) -> Result<u32, PrelinkedGraphCodecError> {
    Ok(u32::from_le_bytes(
      self
        .take(4)?
        .try_into()
        .map_err(|_| invalid("missing u32"))?,
    ))
  }

  fn u64(&mut self) -> Result<u64, PrelinkedGraphCodecError> {
    Ok(u64::from_le_bytes(
      self
        .take(8)?
        .try_into()
        .map_err(|_| invalid("missing u64"))?,
    ))
  }

  fn array_32(&mut self) -> Result<[u8; 32], PrelinkedGraphCodecError> {
    self
      .take(32)?
      .try_into()
      .map_err(|_| invalid("missing 32-byte hash"))
  }

  fn vector_len(
    &mut self,
    maximum: usize,
    kind: &'static str,
  ) -> Result<usize, PrelinkedGraphCodecError> {
    let value = self.u32()? as usize;
    if value > maximum {
      return Err(limit(kind, value, maximum));
    }
    if value > self.remaining() {
      return Err(invalid(format!("{kind} cannot fit in remaining payload")));
    }
    Ok(value)
  }

  fn string(
    &mut self,
    kind: StringKind,
  ) -> Result<String, PrelinkedGraphCodecError> {
    let length = self.vector_len(
      MAX_NAME_BYTES.max(MAX_TOTAL_SOURCE_BYTES),
      "string byte count",
    )?;
    let maximum = match kind {
      StringKind::Name => MAX_NAME_BYTES,
      StringKind::Source => MAX_TOTAL_SOURCE_BYTES,
    };
    if length > maximum {
      return Err(limit("string byte count", length, maximum));
    }
    let total = match kind {
      StringKind::Name => &mut self.name_bytes,
      StringKind::Source => &mut self.source_bytes,
    };
    *total = total
      .checked_add(length)
      .ok_or_else(|| invalid("string byte count overflow"))?;
    let maximum_total = match kind {
      StringKind::Name => MAX_TOTAL_NAME_BYTES,
      StringKind::Source => MAX_TOTAL_SOURCE_BYTES,
    };
    if *total > maximum_total {
      return Err(limit("total string bytes", *total, maximum_total));
    }
    String::from_utf8(self.take(length)?.to_vec()).map_err(|_| invalid("string is not valid UTF-8"))
  }

  fn option_string(
    &mut self,
    kind: StringKind,
  ) -> Result<Option<String>, PrelinkedGraphCodecError> {
    match self.u8()? {
      0 => Ok(None),
      1 => Ok(Some(self.string(kind)?)),
      tag => Err(invalid(format!("invalid optional string tag {tag}"))),
    }
  }

  fn js_string(&mut self) -> Result<js_string::JsString, PrelinkedGraphCodecError> {
    let length = self.vector_len(MAX_NAME_BYTES / 2, "UTF-16 code unit count")?;
    let mut units = Vec::with_capacity(length);
    for _ in 0..length {
      units.push(self.u16()?);
    }
    Ok(units.into())
  }

  fn option_js_string(&mut self) -> Result<Option<js_string::JsString>, PrelinkedGraphCodecError> {
    match self.u8()? {
      0 => Ok(None),
      1 => Ok(Some(self.js_string()?)),
      tag => Err(invalid(format!("invalid optional string tag {tag}"))),
    }
  }

  fn bytes_vec(
    &mut self,
    maximum: usize,
    kind: &'static str,
  ) -> Result<Vec<u8>, PrelinkedGraphCodecError> {
    let length = self.vector_len(maximum, kind)?;
    Ok(self.take(length)?.to_vec())
  }

  fn portable_usize(
    &mut self,
    kind: &'static str,
  ) -> Result<usize, PrelinkedGraphCodecError> {
    usize::try_from(self.u64()?).map_err(|_| invalid(format!("{kind} does not fit usize")))
  }

  fn option_usize(
    &mut self,
    kind: &'static str,
  ) -> Result<Option<usize>, PrelinkedGraphCodecError> {
    match self.u8()? {
      0 => Ok(None),
      1 => Ok(Some(self.portable_usize(kind)?)),
      tag => Err(invalid(format!("invalid optional usize tag {tag}"))),
    }
  }

  fn add_dependency_budget(
    &mut self,
    value: usize,
  ) -> Result<(), PrelinkedGraphCodecError> {
    self.dependency_count = self
      .dependency_count
      .checked_add(value)
      .ok_or_else(|| invalid("dependency count overflow"))?;
    if self.dependency_count > MAX_DEPENDENCIES {
      return Err(limit(
        "dependency count",
        self.dependency_count,
        MAX_DEPENDENCIES,
      ));
    }
    Ok(())
  }

  fn add_binding_budget(
    &mut self,
    value: usize,
  ) -> Result<(), PrelinkedGraphCodecError> {
    self.binding_count = self
      .binding_count
      .checked_add(value)
      .ok_or_else(|| invalid("binding entry count overflow"))?;
    if self.binding_count > MAX_BINDING_ENTRIES {
      return Err(limit(
        "binding entry count",
        self.binding_count,
        MAX_BINDING_ENTRIES,
      ));
    }
    Ok(())
  }

  fn add_bytecode_budget(
    &mut self,
    value: usize,
  ) -> Result<(), PrelinkedGraphCodecError> {
    self.bytecode_bytes = self
      .bytecode_bytes
      .checked_add(value)
      .ok_or_else(|| invalid("bytecode size overflow"))?;
    if self.bytecode_bytes > MAX_TOTAL_BYTECODE_BYTES {
      return Err(limit(
        "total bytecode bytes",
        self.bytecode_bytes,
        MAX_TOTAL_BYTECODE_BYTES,
      ));
    }
    Ok(())
  }

  fn finish(&self) -> Result<(), PrelinkedGraphCodecError> {
    if self.position == self.bytes.len() {
      Ok(())
    } else {
      Err(invalid("trailing bytes in prelinked graph payload"))
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::bytecode::{Opcode, encode_aux16_instruction};
  use crate::prelinked::PRELINKED_MODULE_GRAPH_VERSION;
  use std::collections::HashMap;

  fn graph() -> PrelinkedModuleGraph {
    let source = Arc::new(crate::js!(module, "export const value = 1;").to_string());
    let mut nested = Chunk::new(Arc::clone(&source));
    nested.write_instr(Opcode::PushHandler as u32, 1);
    nested.write_instr(Opcode::Return as u32, 1);
    nested.constants = vec![
      Constant::Number(f64::from_bits(0x7ff8_0000_0000_0042)),
      Constant::BigInt(BigInt::from(-123_i64)),
      Constant::Symbol(SymbolId::well_known(9), Some("symbol".into())),
    ];
    nested.root_slot_types = vec![Some(Type::Object(HashMap::from([
      ("z".into(), Type::Boolean),
      ("a".into(), Type::Array(Box::new(Type::String))),
    ])))];
    nested.root_slot_names = vec![Some("nested".into())];
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

    let mut root = Chunk::new(source);
    root.write_instr(encode_aux16_instruction(Opcode::Undefined, 0, 1), 0);
    root.write_instr(encode_aux16_instruction(Opcode::Return, 0, 1), 0);
    root.root_slot_types = vec![None, None];
    root.root_slot_names = vec![Some("root".into()), None];
    root.root_lexical_tdz = vec![false, false];
    root.root_slot_immutable = vec![false, false];
    root.functions.push(Function {
      name: "nested".into(),
      span: Span::new(0, 24),
      arity: 2,
      length: 1,
      chunk: Arc::new(nested),
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
    PrelinkedModuleGraph {
      format_version: PRELINKED_MODULE_GRAPH_VERSION,
      engine_bytecode_version: BytecodeVersion::RiscV2,
      package_name: "synthetic".into(),
      package_version: "1.0.0".into(),
      source_manifest_hash: [7; 32],
      modules: vec![
        PrelinkedModule {
          specifier: "synthetic:value".into(),
          module_index: 0,
          source_sha256: [1; 32],
          dependencies: vec![],
          exports: vec![PrelinkedExport {
            exported_name: "value".into(),
            binding_id: 0,
          }],
          imports: vec![],
          chunk: Chunk::new(Arc::new(
            crate::js!(module, "export const value = 1;").into(),
          )),
        },
        PrelinkedModule {
          specifier: "synthetic:entry".into(),
          module_index: 1,
          source_sha256: [2; 32],
          dependencies: vec![0],
          exports: vec![PrelinkedExport {
            exported_name: "alias".into(),
            binding_id: 0,
          }],
          imports: vec![PrelinkedImport {
            local_name: "value".into(),
            binding_id: 0,
          }],
          chunk: root,
        },
      ],
      binding_count: 1,
      public_roots: vec![1],
    }
  }

  fn frame(payload: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(HEADER_LEN + payload.len());
    bytes.extend_from_slice(MAGIC);
    bytes.extend_from_slice(&PRELINKED_GRAPH_WIRE_VERSION.to_le_bytes());
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&crc32fast::hash(payload).to_le_bytes());
    bytes.extend_from_slice(payload);
    bytes
  }

  #[test]
  fn round_trip_preserves_every_wire_field_and_is_deterministic() {
    let graph = graph();
    let first = encode(&graph).expect("encode graph");
    assert_eq!(first, encode(&graph).expect("encode graph again"));
    let decoded = decode(&first).expect("decode graph");
    assert_eq!(encode(&decoded).expect("re-encode graph"), first);
    assert_eq!(decoded.modules.len(), 2);
    assert_eq!(decoded.modules[1].imports[0].binding_id, 0);
    assert!(decoded.modules.iter().all(|module| {
      module
        .chunk
        .functions
        .iter()
        .all(|function| Arc::ptr_eq(&module.chunk.source_text, &function.chunk.source_text))
    }));
    assert_eq!(
      decoded.modules[1].chunk.source_text.as_str(),
      "export const value = 1;"
    );
    assert_eq!(
      decoded.modules[1].chunk.functions[0]
        .chunk
        .value_spill_slot_count,
      3
    );
    assert_eq!(decoded.modules[1].chunk.constants.len(), 0);
    assert_eq!(decoded.modules[1].chunk.functions[0].arity, 2);
    assert_eq!(decoded.modules[1].chunk.functions[0].length, 1);
    assert_eq!(
      decoded.modules[1].chunk.functions[0]
        .chunk
        .constants
        .len(),
      3
    );
  }

  #[test]
  fn rejects_mixed_sources_within_a_module() {
    let mut graph = graph();
    Arc::make_mut(&mut graph.modules[1].chunk.functions[0].chunk).source_text =
      Arc::new(crate::js!("function nested() {}").into());
    assert!(
      matches!(encode(&graph), Err(PrelinkedGraphCodecError::InvalidData(message)) if message.contains("share source text"))
    );
  }

  #[test]
  fn equivalent_metadata_order_encodes_canonically() {
    let graph = graph();
    let mut reordered = graph.clone();
    reordered.modules.reverse();
    reordered.public_roots.reverse();
    for module in &mut reordered.modules {
      module.exports.reverse();
      module.imports.reverse();
    }
    assert_eq!(
      encode(&graph).expect("encode graph"),
      encode(&reordered).expect("encode reordered graph")
    );
  }

  #[test]
  fn rejects_corruption_and_trailing_bytes() {
    let mut bytes = encode(&graph()).expect("encode graph");
    let last = bytes.len() - 1;
    bytes[last] ^= 0x80;
    assert!(matches!(
      decode(&bytes),
      Err(PrelinkedGraphCodecError::ChecksumMismatch)
    ));

    let mut bytes = encode(&graph()).expect("encode graph");
    bytes.push(0);
    let payload_len = bytes.len() - HEADER_LEN;
    bytes[10..18].copy_from_slice(&(payload_len as u64).to_le_bytes());
    let checksum = crc32fast::hash(&bytes[HEADER_LEN..]);
    bytes[18..22].copy_from_slice(&checksum.to_le_bytes());
    assert!(
      matches!(decode(&bytes), Err(PrelinkedGraphCodecError::InvalidData(message)) if message.contains("trailing bytes"))
    );
  }

  #[test]
  fn rejects_unknown_wire_version() {
    let mut bytes = encode(&graph()).expect("encode graph");
    bytes[8..10].copy_from_slice(&(PRELINKED_GRAPH_WIRE_VERSION + 1).to_le_bytes());
    assert!(
      matches!(decode(&bytes), Err(PrelinkedGraphCodecError::UnsupportedVersion(version)) if version == PRELINKED_GRAPH_WIRE_VERSION + 1)
    );
  }

  #[test]
  fn rejects_function_arity_that_would_force_an_unbounded_frame() {
    let mut graph = graph();
    graph.modules[1].chunk.functions[0].arity = MAX_FUNCTION_ARITY + 1;
    assert!(matches!(
      encode(&graph),
      Err(PrelinkedGraphCodecError::LimitExceeded {
        kind: "function arity",
        ..
      })
    ));
  }

  #[test]
  fn rejects_oversized_module_count_before_allocation() {
    let mut payload = Vec::new();
    payload.extend_from_slice(&PRELINKED_MODULE_GRAPH_VERSION.to_le_bytes());
    payload.push(BytecodeVersion::RiscV2 as u8);
    payload.extend_from_slice(&1_u32.to_le_bytes());
    payload.push(b'p');
    payload.extend_from_slice(&1_u32.to_le_bytes());
    payload.push(b'v');
    payload.extend_from_slice(&[0; 32]);
    payload.extend_from_slice(&((MAX_MODULES as u32) + 1).to_le_bytes());
    assert!(matches!(
      decode(&frame(&payload)),
      Err(PrelinkedGraphCodecError::LimitExceeded {
        kind: "module count",
        ..
      })
    ));
  }
}
