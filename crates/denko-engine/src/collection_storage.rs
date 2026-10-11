// SPDX-License-Identifier: MPL-2.0

use std::cell::RefCell;
use std::collections::{HashMap, hash_map::RandomState};
use std::hash::{BuildHasher, Hash, Hasher};
use std::rc::Rc;

use crate::runtime_utils::{array_from_values, same_value_zero};
use crate::value::{ArrayData, ObjectData, Value};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CollectionKind {
  Map,
  Set,
}

#[derive(Debug)]
pub(crate) struct CollectionIndex {
  initialized: bool,
  kind: CollectionKind,
  hashes: RandomState,
  positions: HashMap<u64, Vec<usize>>,
  len: usize,
}

impl Clone for CollectionIndex {
  fn clone(&self) -> Self {
    Self {
      initialized: false,
      kind: self.kind,
      hashes: RandomState::new(),
      positions: HashMap::new(),
      len: 0,
    }
  }
}

#[derive(Clone)]
pub(crate) struct CollectionState {
  elements: Rc<RefCell<ArrayData>>,
  properties: Rc<RefCell<ObjectData>>,
  kind: CollectionKind,
}

fn canonicalize(value: Value) -> Value {
  match value {
    Value::Number(number) if number == 0.0 && number.is_sign_negative() => Value::Number(0.0),
    value => value,
  }
}

/// Hash precisely the identities used by SameValueZero, without coercion.
fn hash_key(
  value: &Value,
  hashes: &RandomState,
) -> u64 {
  let mut hash = hashes.build_hasher();
  std::mem::discriminant(value).hash(&mut hash);
  match value {
    Value::Uninitialized | Value::Undefined | Value::Null => {}
    Value::Number(number) => {
      let bits = if number.is_nan() {
        f64::NAN.to_bits()
      } else if *number == 0.0 {
        0
      } else {
        number.to_bits()
      };
      bits.hash(&mut hash);
    }
    Value::String(value) => value.hash(&mut hash),
    Value::Bool(value) => value.hash(&mut hash),
    Value::BigInt(value) => value.hash(&mut hash),
    Value::Symbol(id, _) => id.hash(&mut hash),
    Value::Object(value) => Rc::as_ptr(value).hash(&mut hash),
    Value::Array(value, _) => Rc::as_ptr(value).hash(&mut hash),
    Value::Proxy(value) => Rc::as_ptr(value).hash(&mut hash),
    Value::Closure { props, .. } => Rc::as_ptr(props).hash(&mut hash),
    Value::Builtin(value) => (Rc::as_ptr(value) as *const ()).hash(&mut hash),
    Value::BuiltinObject(_, props) => Rc::as_ptr(props).hash(&mut hash),
    Value::Promise(value, _) => Rc::as_ptr(value).hash(&mut hash),
  }
  hash.finish()
}

fn with_entry_key<T>(
  entry: &Value,
  kind: CollectionKind,
  operation: impl FnOnce(&Value) -> T,
) -> T {
  match kind {
    CollectionKind::Map => {
      let Value::Array(pair, _) = entry else {
        unreachable!("Map slots contain key/value pairs");
      };
      let pair = pair.borrow();
      operation(
        pair
          .first()
          .and_then(Option::as_ref)
          .expect("Map entry key"),
      )
    }
    CollectionKind::Set => operation(entry),
  }
}

impl CollectionState {
  pub(crate) fn new(kind: CollectionKind) -> Self {
    Self::from_value(array_from_values(Vec::new()), kind)
  }

  /// Called only after validating the receiver's private collection slot.
  pub(crate) fn from_value(
    value: Value,
    kind: CollectionKind,
  ) -> Self {
    let Value::Array(elements, properties) = value else {
      unreachable!("validated collection state is an array");
    };
    Self {
      elements,
      properties,
      kind,
    }
  }

  pub(crate) fn value(&self) -> Value {
    Value::Array(Rc::clone(&self.elements), Rc::clone(&self.properties))
  }

  fn with_index<T>(
    &self,
    operation: impl FnOnce(&mut ArrayData, &mut CollectionIndex) -> T,
  ) -> T {
    let mut elements = self.elements.borrow_mut();
    let mut properties = self.properties.borrow_mut();
    // Structured clone copies the authoritative slots into fresh ObjectData.
    // Build from those cloned identities rather than copying pointer hashes.
    if properties
      .collection_index
      .as_ref()
      .is_some_and(|index| !index.initialized)
    {
      properties.collection_index = None;
    }
    let index = properties
      .collection_index
      .get_or_insert_with(|| {
        let mut index = CollectionIndex {
          initialized: true,
          kind: self.kind,
          hashes: RandomState::new(),
          positions: HashMap::new(),
          len: 0,
        };
        for (position, entry) in elements.iter().enumerate() {
          if let Some(entry) = entry {
            let hash = with_entry_key(entry, self.kind, |key| hash_key(key, &index.hashes));
            index
              .positions
              .entry(hash)
              .or_default()
              .push(position);
            index.len += 1;
          }
        }
        Box::new(index)
      });
    debug_assert_eq!(index.kind, self.kind);
    operation(&mut elements, index)
  }

  fn find(
    elements: &ArrayData,
    index: &CollectionIndex,
    hash: u64,
    key: &Value,
  ) -> Option<usize> {
    index
      .positions
      .get(&hash)?
      .iter()
      .copied()
      .find(|position| {
        let entry = elements
          .get(*position)
          .and_then(Option::as_ref)
          .expect("indexed live slot");
        with_entry_key(entry, index.kind, |entry_key| {
          same_value_zero(entry_key, key)
        })
      })
  }

  pub(crate) fn get(
    &self,
    key: &Value,
  ) -> Option<Value> {
    self.with_index(|elements, index| {
      let hash = hash_key(key, &index.hashes);
      let position = Self::find(elements, index, hash, key)?;
      let entry = elements[position]
        .as_ref()
        .expect("indexed live slot");
      match self.kind {
        CollectionKind::Map => {
          let Value::Array(pair, _) = entry else {
            unreachable!("Map entry pair")
          };
          Some(
            pair
              .borrow()
              .get(1)
              .cloned()
              .flatten()
              .expect("Map entry value"),
          )
        }
        CollectionKind::Set => Some(entry.clone()),
      }
    })
  }

  pub(crate) fn has(
    &self,
    key: &Value,
  ) -> bool {
    self.with_index(|elements, index| {
      Self::find(elements, index, hash_key(key, &index.hashes), key).is_some()
    })
  }

  pub(crate) fn set(
    &self,
    key: Value,
    value: Value,
  ) {
    debug_assert_eq!(self.kind, CollectionKind::Map);
    self.insert(key, Some(value));
  }

  pub(crate) fn add(
    &self,
    key: Value,
  ) {
    debug_assert_eq!(self.kind, CollectionKind::Set);
    self.insert(key, None);
  }

  fn insert(
    &self,
    key: Value,
    value: Option<Value>,
  ) {
    let key = canonicalize(key);
    self.with_index(|elements, index| {
      let hash = hash_key(&key, &index.hashes);
      if let Some(position) = Self::find(elements, index, hash, &key) {
        if let Some(value) = value {
          let Some(Value::Array(pair, _)) = &elements[position] else {
            unreachable!("Map entry pair")
          };
          pair.borrow_mut()[1] = Some(value);
        }
        return;
      }
      let entry = match value {
        Some(value) => array_from_values(vec![key, value]),
        None => key,
      };
      let position = elements.len();
      elements.push(Some(entry));
      index
        .positions
        .entry(hash)
        .or_default()
        .push(position);
      index.len += 1;
    });
  }

  pub(crate) fn delete(
    &self,
    key: &Value,
  ) -> bool {
    self.with_index(|elements, index| {
      let hash = hash_key(key, &index.hashes);
      let Some(position) = Self::find(elements, index, hash, key) else {
        return false;
      };
      let positions = index
        .positions
        .get_mut(&hash)
        .expect("indexed hash");
      let bucket_position = positions
        .iter()
        .position(|entry| *entry == position)
        .expect("indexed position");
      positions.swap_remove(bucket_position);
      if positions.is_empty() {
        index.positions.remove(&hash);
      }
      elements[position] = None;
      index.len -= 1;
      true
    })
  }

  pub(crate) fn clear(&self) {
    self.with_index(|elements, index| {
      for entry in elements.iter_mut() {
        *entry = None;
      }
      index.positions.clear();
      index.len = 0;
    });
  }

  pub(crate) fn len(&self) -> usize {
    self.with_index(|_, index| index.len)
  }
}

#[cfg(test)]
mod tests;
