// SPDX-License-Identifier: MPL-2.0
//! Explicit runtime decoders for untrusted JSON input.

use std::fmt;
use std::marker::PhantomData;
use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;

use super::error::RpcDecodeError;
use super::json::JsonValue;

/// A runtime validator for an RPC input.
///
/// Decoders deliberately receive JSON values rather than engine values. This
/// keeps validation deterministic and lets the Rust boundary remain usable
/// independently of the JavaScript Effect integration.
pub trait Decoder: Send + Sync {
  /// Validate and, optionally, normalize an input value.
  fn decode(
    &self,
    input: &JsonValue,
  ) -> Result<JsonValue, RpcDecodeError>;
}

/// A decoder backed by a Rust closure.
pub struct FnDecoder<F> {
  function: F,
}

impl<F> FnDecoder<F> {
  /// Wrap a decoder function.
  pub const fn new(function: F) -> Self {
    Self { function }
  }
}

impl<F> Decoder for FnDecoder<F>
where
  F: Fn(&JsonValue) -> Result<JsonValue, RpcDecodeError> + Send + Sync,
{
  fn decode(
    &self,
    input: &JsonValue,
  ) -> Result<JsonValue, RpcDecodeError> {
    (self.function)(input)
  }
}

/// Construct a decoder from a closure.
pub fn decoder<F>(function: F) -> FnDecoder<F>
where
  F: Fn(&JsonValue) -> Result<JsonValue, RpcDecodeError> + Send + Sync,
{
  FnDecoder::new(function)
}

/// A decoder that accepts only JSON `null`.
#[derive(Clone, Copy, Debug, Default)]
pub struct NoInput;

impl Decoder for NoInput {
  fn decode(
    &self,
    input: &JsonValue,
  ) -> Result<JsonValue, RpcDecodeError> {
    if input.is_null() {
      Ok(JsonValue::Null)
    } else {
      Err(RpcDecodeError::new("procedure does not accept input"))
    }
  }
}

/// A decoder that uses serde for a typed Rust input and normalizes it back to JSON.
pub struct SerdeDecoder<A> {
  marker: PhantomData<fn() -> A>,
}

impl<A> SerdeDecoder<A> {
  /// Construct a serde-backed decoder.
  #[must_use]
  pub const fn new() -> Self {
    Self {
      marker: PhantomData,
    }
  }
}

impl<A> Default for SerdeDecoder<A> {
  fn default() -> Self {
    Self::new()
  }
}

impl<A> fmt::Debug for SerdeDecoder<A> {
  fn fmt(
    &self,
    formatter: &mut fmt::Formatter<'_>,
  ) -> fmt::Result {
    formatter.write_str("SerdeDecoder")
  }
}

impl<A> Decoder for SerdeDecoder<A>
where
  A: DeserializeOwned + Serialize + Send + Sync,
{
  fn decode(
    &self,
    input: &JsonValue,
  ) -> Result<JsonValue, RpcDecodeError> {
    let value = serde_json::from_value::<A>(input.clone())
      .map_err(|error| RpcDecodeError::new(error.to_string()))?;
    serde_json::to_value(value).map_err(|error| RpcDecodeError::new(error.to_string()))
  }
}

/// Share a decoder as an immutable trait object.
pub fn shared_decoder<D>(decoder: D) -> Arc<dyn Decoder>
where
  D: Decoder + 'static,
{
  Arc::new(decoder)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn no_input_rejects_values() {
    assert_eq!(
      NoInput
        .decode(&JsonValue::Null)
        .expect("null input"),
      JsonValue::Null
    );
    assert!(NoInput.decode(&serde_json::json!({})).is_err());
  }

  #[test]
  fn closure_decoder_can_validate_and_normalize() {
    let decoder = decoder(|input| {
      input
        .get("id")
        .and_then(JsonValue::as_str)
        .filter(|id| !id.is_empty())
        .map(|id| serde_json::json!({"id": id}))
        .ok_or_else(|| RpcDecodeError::new("id must be a non-empty string").at("/id"))
    });
    assert_eq!(
      decoder
        .decode(&serde_json::json!({"id": "42"}))
        .expect("valid id"),
      serde_json::json!({"id": "42"})
    );
    assert!(
      decoder
        .decode(&serde_json::json!({"id": 42}))
        .is_err()
    );
  }
}
