// SPDX-License-Identifier: MPL-2.0
use super::frame::Frame;
use super::handler::HandlerRecord;

/// Manages VM call frames as a compact stack component.
#[derive(Default)]
pub(crate) struct CallStack {
  frames: Vec<Frame>,
}

impl CallStack {
  /// Creates a new call stack with a specified capacity.
  pub(crate) fn with_capacity(cap: usize) -> Self {
    Self {
      frames: Vec::with_capacity(cap),
    }
  }

  /// Returns the number of frames on the stack.
  pub(crate) fn len(&self) -> usize {
    self.frames.len()
  }

  /// Returns true if the stack is empty.
  pub(crate) fn is_empty(&self) -> bool {
    self.frames.is_empty()
  }

  /// Pushes a frame onto the stack.
  pub(crate) fn push(
    &mut self,
    frame: Frame,
  ) {
    self.frames.push(frame);
  }

  /// Pops and returns the last frame from the stack.
  pub(crate) fn pop(&mut self) -> Option<Frame> {
    self.frames.pop()
  }

  /// Returns a reference to the last frame without removing it.
  pub(crate) fn last(&self) -> Option<&Frame> {
    self.frames.last()
  }

  /// Returns a mutable reference to the active frame.
  pub(crate) fn last_mut(&mut self) -> Option<&mut Frame> {
    self.frames.last_mut()
  }

  /// Truncates the stack to the specified length.
  pub(crate) fn truncate(
    &mut self,
    len: usize,
  ) {
    self.frames.truncate(len);
  }

  /// Clears all frames from the stack.
  pub(crate) fn clear(&mut self) {
    self.frames.clear();
  }

  /// Returns an iterator over the frames in the stack.
  pub(crate) fn iter(&self) -> std::slice::Iter<'_, Frame> {
    self.frames.iter()
  }

  /// Returns a mutable iterator over the frames in the stack.
  pub(crate) fn iter_mut(&mut self) -> std::slice::IterMut<'_, Frame> {
    self.frames.iter_mut()
  }
}

/// Thin handler stack wrapper that isolates exception handler bookkeeping.
#[derive(Default)]
pub(crate) struct HandlerStack {
  records: Vec<HandlerRecord>,
}

impl HandlerStack {
  /// Creates a new handler stack with a specified capacity.
  pub(crate) fn with_capacity(cap: usize) -> Self {
    Self {
      records: Vec::with_capacity(cap),
    }
  }

  /// Returns the number of handler records on the stack.
  pub(crate) fn len(&self) -> usize {
    self.records.len()
  }

  /// Pushes a handler record onto the stack.
  pub(crate) fn push(
    &mut self,
    record: HandlerRecord,
  ) {
    self.records.push(record);
  }

  /// Pops and returns the last handler record from the stack.
  pub(crate) fn pop(&mut self) -> Option<HandlerRecord> {
    self.records.pop()
  }

  /// Returns a reference to the last handler record without removing it.
  pub(crate) fn last(&self) -> Option<&HandlerRecord> {
    self.records.last()
  }

  pub(crate) fn iter(&self) -> std::slice::Iter<'_, HandlerRecord> {
    self.records.iter()
  }

  pub(crate) fn remove(
    &mut self,
    index: usize,
  ) -> HandlerRecord {
    self.records.remove(index)
  }

  /// Truncates the stack to the specified length.
  pub(crate) fn truncate(
    &mut self,
    len: usize,
  ) {
    self.records.truncate(len);
  }

  /// Clears all handler records from the stack.
  pub(crate) fn clear(&mut self) {
    self.records.clear();
  }
}
