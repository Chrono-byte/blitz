// SPDX-License-Identifier: MPL-2.0
use super::*;
use crate::ecmascript::semantics::byte_views::{byte_view_details, decode_typed_element};
use std::cmp::Ordering;

impl Vm {
  pub(crate) fn try_sort_typed_view(
    &mut self,
    receiver: &Value,
    destination: &Value,
  ) -> Result<bool, RuntimeError> {
    let Some(source) = byte_view_details(receiver) else {
      return Ok(false);
    };
    let Some(target) = byte_view_details(destination) else {
      return Ok(false);
    };
    if source.kind != target.kind
      || source.length != target.length
      || source.data.borrow().packed_bytes().is_none()
      || target.data.borrow().packed_bytes().is_none()
    {
      return Ok(false);
    }
    let width = source.bytes_per_element;
    let mut values = Vec::with_capacity(source.length);
    for start in (0..source.length).step_by(1024) {
      self.poll_execution_interrupt()?;
      let data = source.data.borrow();
      for index in start..(start + 1024).min(source.length) {
        let mut bytes = [0; 8];
        assert!(data.read_bytes(source.byte_offset + index * width, &mut bytes[..width]));
        values.push(bytes);
      }
    }
    let kind = source.kind.as_str_lossy();
    let mut comparisons = 0usize;
    crate::ecmascript::semantics::sorting::sort_values(&mut values, |left, right| {
      comparisons += 1;
      if comparisons.is_multiple_of(1024) {
        self.poll_execution_interrupt()?;
      }
      Ok(match kind {
        "BigInt64Array" => i64::from_le_bytes(*left).cmp(&i64::from_le_bytes(*right)),
        "BigUint64Array" => u64::from_le_bytes(*left).cmp(&u64::from_le_bytes(*right)),
        _ => {
          let Some(Value::Number(left)) = decode_typed_element(kind, &left[..width]) else {
            unreachable!("numeric view")
          };
          let Some(Value::Number(right)) = decode_typed_element(kind, &right[..width]) else {
            unreachable!("numeric view")
          };
          if left.is_nan() {
            if right.is_nan() {
              Ordering::Equal
            } else {
              Ordering::Greater
            }
          } else if right.is_nan() {
            Ordering::Less
          } else if left == 0.0 && right == 0.0 {
            right
              .is_sign_negative()
              .cmp(&left.is_sign_negative())
          } else {
            left.partial_cmp(&right).expect("non-NaN numbers")
          }
        }
      })
    })?;
    for (batch, values) in values.chunks(1024).enumerate() {
      self.poll_execution_interrupt()?;
      let mut data = target.data.borrow_mut();
      for (index, bytes) in values.iter().enumerate() {
        assert!(data.write_bytes(
          target.byte_offset + (batch * 1024 + index) * width,
          &bytes[..width]
        ));
      }
      drop(data);
      self.bump_object_generation(destination);
    }
    Ok(true)
  }

  pub(crate) fn try_fill_typed_view(
    &mut self,
    receiver: &Value,
    start: usize,
    end: usize,
    encoded: &[u8],
  ) -> Result<bool, RuntimeError> {
    let Some(view) = byte_view_details(receiver) else {
      return Ok(false);
    };
    if view.data.borrow().packed_bytes().is_none()
      || end > view.length
      || encoded.len() != view.bytes_per_element
    {
      return Ok(false);
    }
    for batch in (start..end).step_by(1024) {
      self.poll_execution_interrupt()?;
      let mut data = view.data.borrow_mut();
      for index in batch..(batch + 1024).min(end) {
        assert!(data.write_bytes(view.byte_offset + index * view.bytes_per_element, encoded));
      }
      drop(data);
      self.bump_object_generation(receiver);
    }
    Ok(true)
  }

  pub(crate) fn try_copy_within_typed_view(
    &mut self,
    receiver: &Value,
    to: usize,
    from: usize,
    count: usize,
  ) -> Result<bool, RuntimeError> {
    let Some(view) = byte_view_details(receiver) else {
      return Ok(false);
    };
    if view.data.borrow().packed_bytes().is_none()
      || count > view.length.saturating_sub(to)
      || count > view.length.saturating_sub(from)
    {
      return Ok(false);
    }
    let width = view.bytes_per_element;
    let backwards = to > from && to - from < count;
    for batch in (0..count).step_by(1024) {
      self.poll_execution_interrupt()?;
      let mut data = view.data.borrow_mut();
      for offset in batch..(batch + 1024).min(count) {
        let index = if backwards {
          count - 1 - offset
        } else {
          offset
        };
        let mut bytes = [0; 8];
        assert!(data.read_bytes(
          view.byte_offset + (from + index) * width,
          &mut bytes[..width]
        ));
        assert!(data.write_bytes(view.byte_offset + (to + index) * width, &bytes[..width]));
      }
      drop(data);
      self.bump_object_generation(receiver);
    }
    Ok(true)
  }
}
