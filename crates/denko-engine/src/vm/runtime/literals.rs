// SPDX-License-Identifier: MPL-2.0
use crate::errors::RuntimeError;
use crate::gc;
use crate::runtime_utils::{array_length_descriptor, usize_to_f64};
use crate::value::{ArrayData, ObjectData, PropertyDescriptor, PropertyKey, Value};
use crate::vm::Vm;
use crate::vm::runtime::execution::ensure_slot;
use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

impl Vm {
  pub(super) fn build_array_value(
    &mut self,
    values: Vec<Value>,
  ) -> Result<Value, RuntimeError> {
    let n = values.len();
    let elems: Vec<Option<Value>> = values
      .into_iter()
      .map(|value| match value {
        Value::Uninitialized => None,
        other => Some(other),
      })
      .collect();

    let arr_rc = Rc::new(RefCell::new(ArrayData::from(elems)));
    let props: Rc<RefCell<ObjectData>> = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    props.borrow_mut().properties.insert(
      PropertyKey::String("length".to_string().into()),
      array_length_descriptor(n),
    );

    props
      .borrow_mut()
      .set_prototype_value(self.realm().array_prototype());

    let array = Value::Array(Rc::clone(&arr_rc), Rc::clone(&props));
    if crate::trace::enabled() {
      eprintln!(
        "[trace] build_array: created Array arr_ptr={:p} props_ptr={:p} pc={}",
        Rc::as_ptr(&arr_rc),
        Rc::as_ptr(&props),
        self.execution.active.pc.saturating_sub(1)
      );
    }
    Ok(array)
  }

  pub(super) fn build_object_value(
    &mut self,
    values: Vec<Value>,
  ) -> Result<Value, RuntimeError> {
    if !values.len().is_multiple_of(2) {
      return Err(RuntimeError::from(
        "object literal operand count is not even",
      ));
    }
    let n = values.len() / 2;
    let mut map = indexmap::IndexMap::with_capacity(n);
    let mut values = values.into_iter();
    while let Some(key_val) = values.next() {
      let value = values
        .next()
        .expect("validated object literal operand pairs");
      let key = self.to_property_key(&key_val)?;
      map.insert(key, PropertyDescriptor::ordinary_data(value));
    }
    let mut data = ObjectData::with_prototype(self.realm().object_prototype());
    data.properties = map;

    let obj_rc = gc::track_object(Rc::new(RefCell::new(data)));
    let object = Value::Object(Rc::clone(&obj_rc));
    if crate::trace::enabled() {
      eprintln!(
        "[trace] build_object: created Object obj_ptr={:p} pc={}",
        Rc::as_ptr(&obj_rc),
        self.execution.active.pc.saturating_sub(1)
      );
    }
    Ok(object)
  }

  #[allow(clippy::too_many_lines)]
  pub(super) fn make_closure_value(
    &mut self,
    idx: usize,
  ) -> Result<Value, RuntimeError> {
    let owner = &self.execution.active.chunk;
    let proto = owner
      .functions
      .get(idx)
      .ok_or("Bad function index")?;
    let cached = self
      .caches
      .closure_prototypes
      .entry((Arc::as_ptr(owner) as usize, idx))
      .or_insert_with(|| {
        let mut properties = indexmap::IndexMap::with_capacity(3);
        properties.insert(
          PropertyKey::String("length".into()),
          PropertyDescriptor::builtin_metadata(Value::Number(usize_to_f64(proto.length))),
        );
        let name = if matches!(proto.name.as_str(), "<anonymous>" | "<arrow>" | "<method>") {
          "".into()
        } else {
          proto.name.as_str().into()
        };
        properties.insert(
          PropertyKey::String("name".into()),
          PropertyDescriptor::builtin_metadata(Value::String(name)),
        );
        let mut constructor_properties = indexmap::IndexMap::new();
        if proto.has_prototype_property {
          properties.insert(
            PropertyKey::String("prototype".into()),
            PropertyDescriptor::Data {
              value: Value::Undefined,
              writable: true,
              enumerable: false,
              configurable: false,
            },
          );
          if !proto.is_generator {
            constructor_properties.insert(
              PropertyKey::String("constructor".into()),
              PropertyDescriptor::builtin_data(Value::Undefined),
            );
          }
        }
        crate::vm::state::CachedClosurePrototype {
          _owner: Arc::clone(owner),
          prototype: Rc::new(proto.clone()),
          properties,
          constructor_properties,
        }
      });
    let closure_proto = Rc::clone(&cached.prototype);
    let metadata_properties = cached.properties.clone();
    let constructor_properties = cached.constructor_properties.clone();
    let has_prototype_property = closure_proto.has_prototype_property;

    let props: Rc<RefCell<ObjectData>> = gc::track_object(Rc::new(RefCell::new(
      ObjectData::with_prototype(self.realm_state.realm.function_prototype()),
    )));

    let mut env_cells = Vec::with_capacity(closure_proto.upvalues.len());
    if crate::trace::enabled() {
      eprintln!(
        "[trace] make_closure proto='{}' upvalues={} current_env_len={}",
        closure_proto.name,
        closure_proto.upvalues.len(),
        self
          .execution
          .active
          .closure_env
          .as_ref()
          .map_or(0, |env| env.borrow().len())
      );
      for (i, upvalue) in closure_proto.upvalues.iter().enumerate() {
        eprintln!(
          "[trace]   upvalue[{}] local={} index={}",
          i, upvalue.is_local, upvalue.index
        );
      }
    }
    for upvalue in &closure_proto.upvalues {
      let cell = if upvalue.is_local {
        let slot = upvalue.index as usize;
        ensure_slot(&mut self.execution.active.locals, slot);
        self
          .execution
          .active
          .locals
          .get(slot)
          .cloned()
          .ok_or("Bad upvalue local slot")?
      } else {
        let env = self
          .execution
          .active
          .closure_env
          .as_ref()
          .ok_or("Bad enclosing closure env")?;
        let env_guard = env.borrow();
        env_guard
          .get(upvalue.index as usize)
          .cloned()
          .ok_or_else(|| {
            if crate::trace::enabled() {
              eprintln!(
                "[diag] make_closure bad enclosing index={} env_len={} proto='{}'",
                upvalue.index,
                env_guard.len(),
                closure_proto.name
              );
              for (i, spec) in closure_proto.upvalues.iter().enumerate() {
                eprintln!(
                  "[diag]   upvalue[{}] local={} index={}",
                  i, spec.is_local, spec.index
                );
              }
            }
            "Bad enclosing upvalue index"
          })?
      };
      env_cells.push(cell);
    }

    if closure_proto.captures_this {
      props.borrow_mut().set_internal_slot(
        "[[LexicalNewTarget]]",
        self
          .current_new_target()
          .unwrap_or(Value::Undefined),
      );
    }
    let closure = Value::Closure {
      proto: Rc::clone(&closure_proto),
      env: self
        .memory
        .allocate_closure_environment(env_cells),
      lexical_this: if closure_proto.captures_this {
        Some(Box::new(self.execution.active.this_value.clone()))
      } else {
        None
      },
      props: Rc::clone(&props),
    };
    props
      .borrow_mut()
      .set_captured_lexical_env(Some(Rc::clone(&self.execution.active.lexical_env)));
    props.borrow_mut().set_internal_slot(
      "[[RealmGlobals]]",
      Value::Object(Rc::clone(&self.realm_state.globals)),
    );
    if closure_proto.is_generator && closure_proto.is_async {
      props.borrow_mut().set_prototype_value(
        self
          .realm_state
          .realm
          .async_generator_function_prototype(),
      );
    } else if closure_proto.is_generator {
      props.borrow_mut().set_prototype_value(
        self
          .realm_state
          .realm
          .generator_function_prototype(),
      );
    } else if closure_proto.is_async {
      props
        .borrow_mut()
        .set_prototype_value(self.realm_state.realm.async_function_prototype());
    }

    if closure_proto.captures_this
      && let Some(home_object) = self.current_method_home_object()
    {
      props.borrow_mut().properties.insert(
        PropertyKey::String("[[HomeObject]]".to_string().into()),
        PropertyDescriptor::ordinary_data(home_object),
      );
    }

    if props.borrow().properties.is_empty() {
      props.borrow_mut().properties = metadata_properties;
    } else {
      props
        .borrow_mut()
        .properties
        .extend(metadata_properties);
    }

    if has_prototype_property {
      let proto_obj = if closure_proto.is_generator {
        Value::Object(gc::track_object(Rc::new(RefCell::new(
          ObjectData::with_prototype(if closure_proto.is_async {
            self.realm_state.realm.async_generator_prototype()
          } else {
            self.realm_state.realm.generator_prototype()
          }),
        ))))
      } else {
        let object_proto = self
          .realm_state
          .realm
          .intrinsics
          .object_prototype
          .clone()
          .unwrap_or(Value::Null);
        let proto_obj_map: Rc<RefCell<ObjectData>> = gc::track_object(Rc::new(RefCell::new(
          ObjectData::with_prototype(object_proto),
        )));
        Value::Object(proto_obj_map)
      };

      if !closure_proto.is_generator
        && let Value::Object(proto_obj_map) = &proto_obj
      {
        let mut prototype = proto_obj_map.borrow_mut();
        prototype.properties = constructor_properties;
        if let Some((_, PropertyDescriptor::Data { value, .. })) =
          prototype.properties.get_index_mut(0)
        {
          *value = closure.clone();
        }
      }

      let mut properties = props.borrow_mut();
      let index = properties.properties.len() - 1;
      if let Some((_, PropertyDescriptor::Data { value, .. })) =
        properties.properties.get_index_mut(index)
      {
        *value = proto_obj;
      }
    }

    if crate::trace::enabled() {
      if let Value::Closure { props, .. } = &closure {
        eprintln!(
          "[trace] make_closure: created Closure props_ptr={:p} proto_idx={} proto_chunk_ptr={:p} pc={}",
          Rc::as_ptr(props),
          idx,
          Arc::as_ptr(&closure_proto.chunk),
          self.execution.active.pc.saturating_sub(1)
        );
      }
      if has_prototype_property
        && let Value::Closure { props, .. } = &closure
        && let Some(PropertyDescriptor::Data { value, .. }) = props
          .borrow()
          .properties
          .get(&PropertyKey::String("prototype".to_string().into()))
        && let Value::Object(proto_obj_map) = value
      {
        eprintln!(
          "[trace] make_closure: created proto_obj props_ptr={:p}",
          Rc::as_ptr(proto_obj_map)
        );
      }
    }
    Ok(closure)
  }
}

#[cfg(test)]
mod closure_metadata_tests {
  use super::*;

  #[test]
  fn plain_data_literals_preserve_order_descriptors_and_special_forms() {
    let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
    evaluate(
      &mut vm,
      crate::js!(
        r#"
      var events = '';
      Object.defineProperty(Object.prototype, 'field', {
        set: function() { throw 'inherited setter'; }, configurable: true
      });
      function next(value) { events += value; return value; }
      var shorthand = 4;
      var object = { field: next('a'), 2: next('b'), field: next('c'), shorthand, nested: { x: 7 } };
      var descriptor = Object.getOwnPropertyDescriptor(object, 'field');
      var result = events === 'abc' && object.field === 'c' && object[2] === 'b'
        && object.shorthand === 4 && object.nested.x === 7
        && Object.keys(object).join(',') === '2,field,shorthand,nested'
        && descriptor.writable && descriptor.enumerable && descriptor.configurable;
      delete Object.prototype.field;
      var named = { method: function() {}, arrow: () => 1 };
      result = result && named.method.name === 'method' && named.arrow.name === 'arrow';
      var base = { value: 13 };
      var special = { __proto__: base, method() { return super.value; }, get read() { return 9; } };
      result = result && Object.getPrototypeOf(special) === base && special.method() === 13 && special.read === 9;
      var key = { toString() { events += 'k'; return 'computed'; } };
      var computed = { [key]: next('v') };
      result = result && events === 'abckv' && computed.computed === 'v';
      function fail() { throw 17; }
      try { var aborted = { a: next('x'), b: fail(), c: next('z') }; }
      catch (error) { result = result && error === 17 && events === 'abckvx'; }
    "#
      ),
    );
    assert_eq!(vm.get_global("result"), Some(Value::Bool(true)));
  }

  #[test]
  fn literal_prototypes_ignore_replaced_global_constructors() {
    let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
    evaluate(
      &mut vm,
      crate::js!(
        r#"
      var getPrototypeOf = Object.getPrototypeOf;
      var arrayPrototype = Array.prototype;
      var objectPrototype = Object.prototype;
      var reads = 0;
      var replacement = {get prototype() { reads++; throw 1; }};
      Array = replacement;
      Object = replacement;
      var array = [1, 2];
      var object = {a: 1, a: 2};
      var literalPrototypesOk = getPrototypeOf(array) === arrayPrototype
        && getPrototypeOf(object) === objectPrototype && reads === 0 && object.a === 2;
    "#
      ),
    );
    assert_eq!(
      vm.get_global("literalPrototypesOk"),
      Some(Value::Bool(true))
    );
  }

  #[test]
  fn closure_prototypes_use_realm_intrinsics_after_global_replacement() {
    let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
    evaluate(
      &mut vm,
      crate::js!(
        r#"
      var originalFunction = Function;
      var intrinsic = Function.prototype;
      var prototypeReads = 0;
      Function = { get prototype() { prototypeReads++; return {}; } };
      var ordinary = function named(value) {};
      var arrow = value => value;
      var metadata = Object.getOwnPropertyDescriptor(ordinary, 'name');
      var result = Object.getPrototypeOf(ordinary) === intrinsic
        && Object.getPrototypeOf(arrow) === intrinsic && prototypeReads === 0
        && ordinary.prototype.constructor === ordinary
        && Reflect.ownKeys(ordinary).join(',') === 'length,name,prototype'
        && metadata.value === 'named' && !metadata.writable && !metadata.enumerable
        && metadata.configurable && ordinary.length === 1;
      Function = originalFunction;
    "#
      ),
    );
    assert_eq!(vm.get_global("result"), Some(Value::Bool(true)));
  }

  fn evaluate(
    vm: &mut Vm,
    source: &str,
  ) {
    let session = crate::parser::Parser::parse_source(source).expect("parse");
    let code = crate::compiler::compile_repl_session(&session).expect("compile");
    vm.run(code).expect("evaluate");
  }

  #[test]
  fn fresh_closures_share_metadata_and_keep_distinct_state() {
    let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
    evaluate(
      &mut vm,
      crate::js!(
        r#"
      function make(value) { return function read() { return value++; }; }
      var first = make(10);
      var second = make(20);
      first.extra = 42;
      first.prototype.extra = 99;
      var independent = first !== second && first.prototype !== second.prototype
        && first.prototype.constructor === first && second.prototype.constructor === second
        && second.extra === undefined && second.prototype.extra === undefined
        && first() === 10 && second() === 20 && first() === 11 && second() === 21;
      Object.defineProperty(first, 'name', { value: 'changed' });
      independent = independent && second.name === 'read';
    "#
      ),
    );
    assert_eq!(vm.get_global("independent"), Some(Value::Bool(true)));
    let first = vm.get_global("first").expect("first closure");
    let second = vm.get_global("second").expect("second closure");
    if let (Value::Closure { proto: first, .. }, Value::Closure { proto: second, .. }) =
      (first, second)
    {
      assert!(Rc::ptr_eq(&first, &second));
    } else {
      unreachable!("evaluated function expressions must create closures");
    }
  }

  #[test]
  fn metadata_cache_separates_chunks_and_clears_on_reset() {
    let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
    evaluate(
      &mut vm,
      crate::js!("var first = function () { return 11; };"),
    );
    evaluate(
      &mut vm,
      crate::js!(
        "var second = function () { return 22; }; var separate = first() === 11 && second() === 22;"
      ),
    );
    assert_eq!(vm.get_global("separate"), Some(Value::Bool(true)));
    assert_eq!(vm.caches.closure_prototypes.len(), 2);
    assert!(
      vm.fork_execution()
        .caches
        .closure_prototypes
        .is_empty()
    );
    vm.reset(&std::collections::HashSet::new());
    assert!(vm.caches.closure_prototypes.is_empty());
  }
}

#[cfg(test)]
mod async_generator_intrinsic_tests {
  use super::*;

  #[test]
  fn async_generator_intrinsics_have_distinct_prototypes_and_reject_invalid_receivers() {
    let source = crate::js!(
      r#"
      var f = async function* () {};
      var functionPrototype = Object.getPrototypeOf(f);
      var generatorPrototype = Object.getPrototypeOf(f.prototype);
      var iteratorPrototype = Object.getPrototypeOf(generatorPrototype);
      var ctor = functionPrototype.constructor;
      var dynamic = ctor('value', 'yield value');
      var tags = Object.prototype.toString.call(f) === '[object AsyncGeneratorFunction]'
        && Object.prototype.toString.call(f.prototype) === '[object AsyncGenerator]';
      var graph = functionPrototype.prototype === generatorPrototype
        && generatorPrototype.constructor === functionPrototype
        && Object.getPrototypeOf(functionPrototype) === Function.prototype
        && Object.getPrototypeOf(ctor) === Function
        && Object.getPrototypeOf(dynamic) === functionPrototype
        && Object.getPrototypeOf(dynamic.prototype) === generatorPrototype
        && iteratorPrototype[Symbol.asyncIterator].call(f) === f
        && !(Symbol.iterator in f.prototype);
      var descriptors = true;
      for (var name of ['next', 'return', 'throw']) {
        var d = Object.getOwnPropertyDescriptor(generatorPrototype, name);
        descriptors = descriptors && d.writable && !d.enumerable && d.configurable
          && d.value.name === name && d.value.length === 1;
      }
      var rejected = 0;
      for (var name of ['next', 'return', 'throw']) {
        var promise = generatorPrototype[name].call({});
        if (!(promise instanceof Promise)) throw Error('missing Promise');
        promise.catch(function (error) { if (error instanceof TypeError) rejected++; });
      }
    "#
    );
    let session = crate::parser::Parser::parse_source(source).expect("parse");
    let code = crate::compiler::compile_repl_session(&session).expect("compile");
    let mut vm = Vm::new(Rc::new(RefCell::new(ObjectData::new())));
    vm.run(code).expect("evaluate prototype checks");
    vm.flush_microtasks()
      .expect("settle rejected calls");
    for name in ["tags", "graph", "descriptors"] {
      assert!(
        matches!(vm.get_global(name), Some(Value::Bool(true))),
        "{name}"
      );
    }
    assert!(matches!(
      vm.get_global("rejected"),
      Some(Value::Number(3.0))
    ));
  }
}
