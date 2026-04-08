use super::frame::Frame;
use super::handler::HandlerRecord;
use crate::value::{PropertyKey, Value};

/// Manages VM call frames as a compact stack component.
#[derive(Default)]
pub(crate) struct CallStack {
    frames: Vec<Frame>,
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

    pub(crate) fn push(&mut self, frame: Frame) {
        self.frames.push(frame);
    }

    pub(crate) fn pop(&mut self) -> Option<Frame> {
        self.frames.pop()
    }

    pub(crate) fn last(&self) -> Option<&Frame> {
        self.frames.last()
    }

    pub(crate) fn truncate(&mut self, len: usize) {
        self.frames.truncate(len);
    }

    pub(crate) fn clear(&mut self) {
        self.frames.clear();
    }

    pub(crate) fn iter(&self) -> std::slice::Iter<'_, Frame> {
        self.frames.iter()
    }

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
    pub(crate) fn with_capacity(cap: usize) -> Self {
        Self {
            records: Vec::with_capacity(cap),
        }
    }

    pub(crate) fn len(&self) -> usize {
        self.records.len()
    }

    pub(crate) fn push(&mut self, record: HandlerRecord) {
        self.records.push(record);
    }

    pub(crate) fn pop(&mut self) -> Option<HandlerRecord> {
        self.records.pop()
    }

    pub(crate) fn last(&self) -> Option<&HandlerRecord> {
        self.records.last()
    }

    pub(crate) fn truncate(&mut self, len: usize) {
        self.records.truncate(len);
    }

    pub(crate) fn clear(&mut self) {
        self.records.clear();
    }
}

/// ECMAScript reference record (base, referenced name, strict mode flag).
#[derive(Clone)]
pub(crate) struct ReferenceRecord {
    pub(crate) base: Value,
    pub(crate) referenced_name: PropertyKey,
    pub(crate) strict: bool,
}

impl ReferenceRecord {
    pub(crate) fn new(base: Value, referenced_name: PropertyKey, strict: bool) -> Self {
        Self {
            base,
            referenced_name,
            strict,
        }
    }
}

/// Stack storing pending reference records for future assignments.
#[derive(Default)]
pub(crate) struct ReferenceStack {
    inner: Vec<ReferenceRecord>,
}

impl ReferenceStack {
    pub(crate) fn new() -> Self {
        Self { inner: Vec::new() }
    }

    pub(crate) fn push(&mut self, record: ReferenceRecord) {
        self.inner.push(record);
    }

    pub(crate) fn pop(&mut self) -> Option<ReferenceRecord> {
        self.inner.pop()
    }

    pub(crate) fn last(&self) -> Option<&ReferenceRecord> {
        self.inner.last()
    }
}
