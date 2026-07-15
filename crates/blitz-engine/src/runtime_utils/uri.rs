#[cfg(test)]
mod tests {
  use crate::Vm;
  use crate::runtime_utils::*;
  use crate::value::ObjectData;
  use crate::value::Value;
  use std::cell::RefCell;
  use std::rc::Rc;

  fn test_vm() -> Vm {
    Vm::new(Rc::new(RefCell::new(ObjectData::new())))
  }

  #[test]
  fn decode_uri_keeps_reserved_escapes() {
    let mut vm = test_vm();

    let preserved = global_decode_uri(&mut vm, &[Value::String("%3B%2F%3F".to_string())])
      .expect("decodeURI should succeed");
    assert_eq!(preserved, Value::String("%3B%2F%3F".to_string()));

    let decoded = global_decode_uri_component(&mut vm, &[Value::String("%3B%2F%3F".to_string())])
      .expect("decodeURIComponent should succeed");
    assert_eq!(decoded, Value::String(";/?".to_string()));
  }

  #[test]
  fn decode_uri_streams_literal_utf8_bytes() {
    let mut vm = test_vm();
    let value = global_decode_uri(&mut vm, &[Value::String("héllo".to_string())])
      .expect("decodeURI should accept raw utf8");
    assert_eq!(value, Value::String("héllo".to_string()));
  }
}
