// SPDX-License-Identifier: MPL-2.0
use super::frame::CallFrame;
use super::handler::HandlerRecord;

#[derive(Default)]
pub(crate) struct CallStack {
  frames: Vec<CallFrame>,
}

impl CallStack {
  pub(crate) fn with_capacity(cap: usize) -> Self {
    Self {
      frames: Vec::with_capacity(cap),
    }
  }

  pub(crate) fn len(&self) -> usize {
    self.frames.len()
  }

  pub(crate) fn is_empty(&self) -> bool {
    self.frames.is_empty()
  }

  pub(crate) fn push(
    &mut self,
    frame: CallFrame,
  ) {
    self.frames.push(frame);
  }

  pub(crate) fn pop(&mut self) -> Option<CallFrame> {
    self.frames.pop()
  }

  pub(crate) fn last(&self) -> Option<&CallFrame> {
    self.frames.last()
  }

  pub(crate) fn last_mut(&mut self) -> Option<&mut CallFrame> {
    self.frames.last_mut()
  }

  pub(crate) fn truncate(
    &mut self,
    len: usize,
  ) {
    self.frames.truncate(len);
  }

  pub(crate) fn clear(&mut self) {
    self.frames.clear();
  }

  pub(crate) fn iter(&self) -> std::slice::Iter<'_, CallFrame> {
    self.frames.iter()
  }

  pub(crate) fn iter_mut(&mut self) -> std::slice::IterMut<'_, CallFrame> {
    self.frames.iter_mut()
  }
}

#[derive(Default)]
pub(crate) struct HandlerStack {
  records: Vec<HandlerRecord>,
}

impl HandlerStack {
  pub(crate) fn with_capacity(cap: usize) -> Self {
    Self {
      records: Vec::with_capacity(cap),
    }
  }

  pub(crate) fn len(&self) -> usize {
    self.records.len()
  }

  pub(crate) fn push(
    &mut self,
    record: HandlerRecord,
  ) {
    self.records.push(record);
  }

  pub(crate) fn pop(&mut self) -> Option<HandlerRecord> {
    self.records.pop()
  }

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

  pub(crate) fn truncate(
    &mut self,
    len: usize,
  ) {
    self.records.truncate(len);
  }

  pub(crate) fn clear(&mut self) {
    self.records.clear();
  }
}
