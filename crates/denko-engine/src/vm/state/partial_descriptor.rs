// SPDX-License-Identifier: MPL-2.0
use crate::value::Value;

#[derive(Clone, Debug, Default)]
pub(crate) struct PartialDescriptor {
  pub(crate) value: Option<Value>,
  pub(crate) writable: Option<bool>,
  pub(crate) enumerable: Option<bool>,
  pub(crate) configurable: Option<bool>,
  pub(crate) get: Option<Value>,
  pub(crate) set: Option<Value>,
  pub(crate) has_value: bool,
  pub(crate) has_writable: bool,
  pub(crate) has_enumerable: bool,
  pub(crate) has_configurable: bool,
  pub(crate) has_get: bool,
  pub(crate) has_set: bool,
}
