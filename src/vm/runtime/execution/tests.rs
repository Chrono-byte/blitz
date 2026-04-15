use super::*;
use crate::compiler;
use crate::parser::Parser;
use crate::runtime_utils;
use crate::runtime_utils::strict_equal;
use crate::typecheck;
use crate::value::ArrayData;
use std::cell::RefCell;
use std::rc::Rc;

fn test_globals() -> Rc<RefCell<ObjectData>> {
    let globals = Rc::new(RefCell::new(ObjectData::new()));
    globals.borrow_mut().properties.insert(
        PropertyKey::String("globalThis".to_string()),
        PropertyDescriptor::data(Value::Object(globals.clone())),
    );
    runtime_utils::install_minimal_kernel_constructors(&globals);
    globals
}

fn run_script(src: &str) -> (Vm, Rc<RefCell<ObjectData>>) {
    let session = Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals.clone());
    vm.run(chunk).expect("run ok");
    (vm, globals)
}

#[test]
fn object_get_own_property_descriptor_reports_data_flags() {
    let (vm, _globals) = run_script(
        r#"
        var obj = {};
        Object.defineProperty(obj, "prop", {
          value: "prop",
          enumerable: false,
          writable: false,
          configurable: false,
        });
        var desc = Object.getOwnPropertyDescriptor(obj, "prop");
        globalThis.desc_value = desc.value;
        globalThis.desc_enumerable = desc.enumerable;
        globalThis.desc_writable = desc.writable;
        globalThis.desc_configurable = desc.configurable;
        "#,
    );

    assert_eq!(
        vm.get_global("desc_value"),
        Some(Value::String("prop".to_string()))
    );
    assert_eq!(vm.get_global("desc_enumerable"), Some(Value::Bool(false)));
    assert_eq!(vm.get_global("desc_writable"), Some(Value::Bool(false)));
    assert_eq!(vm.get_global("desc_configurable"), Some(Value::Bool(false)));
}

#[test]
fn object_get_own_property_descriptor_works_when_extracted() {
    let (vm, _globals) = run_script(
        r#"
        var obj = { a: 1, b: true };
        var getOwnPropertyDescriptor = Object.getOwnPropertyDescriptor;
        var desc = getOwnPropertyDescriptor(obj, "a");
        globalThis.desc_value = desc && desc.value;
        globalThis.desc_enumerable = desc && desc.enumerable;
        "#,
    );

    assert_eq!(vm.get_global("desc_value"), Some(Value::Number(1.0)));
    assert_eq!(vm.get_global("desc_enumerable"), Some(Value::Bool(true)));
}

#[test]
fn reflect_own_keys_matches_get_own_property_descriptor() {
    let (vm, _globals) = run_script(
        r#"
        var obj = { a: 1, b: true };
        var keys = Reflect.ownKeys(obj);
        globalThis.k0 = keys[0];
        globalThis.k1 = keys[1];
        globalThis.d0 = Object.getOwnPropertyDescriptor(obj, keys[0]) !== undefined;
        globalThis.d1 = Object.getOwnPropertyDescriptor(obj, keys[1]) !== undefined;
        "#,
    );

    assert_eq!(vm.get_global("k0"), Some(Value::String("a".to_string())));
    assert_eq!(vm.get_global("k1"), Some(Value::String("b".to_string())));
    assert_eq!(vm.get_global("d0"), Some(Value::Bool(true)));
    assert_eq!(vm.get_global("d1"), Some(Value::Bool(true)));
}

#[test]
fn builtin_object_property_lookup_uses_props_and_prototype() {
    let proto = Rc::new(RefCell::new(ObjectData::new()));
    proto.borrow_mut().properties.insert(
        PropertyKey::String("inherited".to_string()),
        PropertyDescriptor::data(Value::Number(7.0)),
    );

    let props = Rc::new(RefCell::new(ObjectData::with_prototype(Value::Object(
        proto.clone(),
    ))));
    props.borrow_mut().properties.insert(
        PropertyKey::String("own".to_string()),
        PropertyDescriptor::data(Value::Number(3.0)),
    );

    let builtin = Value::BuiltinObject(Rc::new(|_, _| Ok(Value::Undefined)), props);

    assert!(matches!(
        get_property(&builtin, &PropertyKey::String("own".to_string()), 0).unwrap(),
        Value::Number(3.0)
    ));
    assert!(matches!(
        get_property(&builtin, &PropertyKey::String("inherited".to_string()), 0).unwrap(),
        Value::Number(7.0)
    ));
}

#[test]
fn set_property_tracked_mutates_builtin_object_props() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let props = Rc::new(RefCell::new(ObjectData::new()));
    let builtin = Value::BuiltinObject(Rc::new(|_, _| Ok(Value::Undefined)), props.clone());

    vm.set_property_tracked(
        &builtin,
        &PropertyKey::String("prototype".to_string()),
        Value::Number(11.0),
    )
    .unwrap_or_default();

    let stored = props
        .borrow()
        .properties
        .get(&PropertyKey::String("prototype".to_string()))
        .cloned();
    assert!(matches!(
        stored,
        Some(PropertyDescriptor::Data {
            value: Value::Number(_),
            ..
        })
    ));
}

#[test]
fn get_property_ic_distinguishes_keys_at_same_slot() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);

    let obj_data = Rc::new(RefCell::new(ObjectData::new()));
    obj_data.borrow_mut().properties.insert(
        PropertyKey::String("x".to_string()),
        PropertyDescriptor::data(Value::Number(1.0)),
    );
    obj_data.borrow_mut().properties.insert(
        PropertyKey::String("y".to_string()),
        PropertyDescriptor::data(Value::Number(2.0)),
    );
    let obj = Value::Object(obj_data.clone());

    vm.chunk = Rc::new(Chunk {
        code: vec![0],
        constants: vec![],
        functions: vec![],
        lines: vec![1],
        source_text: Rc::new(String::new()),
        root_slot_types: vec![],
        root_slot_names: vec![],
        root_lexical_tdz: vec![],
        root_strict: false,
        allow_new_target: false,
        allow_super: false,
    });
    vm.ip = 0;
    vm.stack.push(obj.clone());
    vm.stack.push(Value::String("x".to_string()));
    vm.get_property_ic().unwrap();
    assert!(matches!(vm.pop().unwrap(), Value::Number(1.0)));

    vm.ip = 0;
    vm.stack.push(obj);
    vm.stack.push(Value::String("y".to_string()));
    vm.get_property_ic().unwrap();
    assert!(matches!(vm.pop().unwrap(), Value::Number(2.0)));
}

#[test]
fn get_property_ic_invalidates_after_mutation() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);

    let obj_data = Rc::new(RefCell::new(ObjectData::new()));
    let key = PropertyKey::String("x".to_string());
    obj_data
        .borrow_mut()
        .properties
        .insert(key.clone(), PropertyDescriptor::data(Value::Number(1.0)));
    let obj = Value::Object(obj_data.clone());

    vm.chunk = Rc::new(Chunk {
        code: vec![0],
        constants: vec![],
        functions: vec![],
        lines: vec![1],
        source_text: Rc::new(String::new()),
        root_slot_types: vec![],
        root_slot_names: vec![],
        root_lexical_tdz: vec![],
        root_strict: false,
        allow_new_target: false,
        allow_super: false,
    });
    vm.ip = 0;
    vm.stack.push(obj.clone());
    vm.stack.push(Value::String("x".to_string()));
    vm.get_property_ic().unwrap();
    assert!(matches!(vm.pop().unwrap(), Value::Number(1.0)));

    vm.set_property_tracked(&obj, &key, Value::Number(7.0))
        .unwrap();

    vm.ip = 0;
    vm.stack.push(obj);
    vm.stack.push(Value::String("x".to_string()));
    vm.get_property_ic().unwrap();
    assert!(matches!(vm.pop().unwrap(), Value::Number(7.0)));
}

#[test]
fn same_value_treats_nan_as_equal() {
    assert!(same_value(
        &Value::Number(f64::NAN),
        &Value::Number(f64::NAN)
    ));
}

#[test]
fn same_value_distinguishes_signed_zero() {
    assert!(!same_value(&Value::Number(0.0), &Value::Number(-0.0)));
    assert!(same_value(&Value::Number(-0.0), &Value::Number(-0.0)));
}

#[test]
fn object_assign_copies_properties_and_returns_target() {
    let (mut vm, _) = run_script(
        r#"
            var target = { a: 1 };
            var result = Object.assign(target, { b: 2 }, null, { c: 3 });
        "#,
    );

    let target = vm.get_global("target").expect("target");
    let result = vm.get_global("result").expect("result");
    assert!(strict_equal(&target, &result));
    let a = vm
        .get_property_host(&target, &PropertyKey::String("a".to_string()))
        .unwrap();
    let b = vm
        .get_property_host(&target, &PropertyKey::String("b".to_string()))
        .unwrap();
    let c = vm
        .get_property_host(&target, &PropertyKey::String("c".to_string()))
        .unwrap();
    assert!(matches!(a, Value::Number(n) if (n - 1.0).abs() < f64::EPSILON));
    assert!(matches!(b, Value::Number(n) if (n - 2.0).abs() < f64::EPSILON));
    assert!(matches!(c, Value::Number(n) if (n - 3.0).abs() < f64::EPSILON));
}

#[test]
fn object_from_entries_set_prototype_of_and_to_locale_string_work() {
    let (mut vm, _) = run_script(
        r#"
            var entries = [['a', 1], ['b', 2]];
            var created = Object.fromEntries(entries);
            var proto = { marker: true };
            var target = {};
            var proto_result = Object.setPrototypeOf(target, proto);
            var locale = ({}).toLocaleString();
        "#,
    );

    match vm.get_global("created") {
        Some(Value::Object(obj)) => {
            assert!(matches!(
                vm.get_property_host(&Value::Object(obj.clone()), &PropertyKey::String("a".to_string())).ok(),
                Some(Value::Number(n)) if (n - 1.0).abs() < f64::EPSILON
            ));
            assert!(matches!(
                vm.get_property_host(&Value::Object(obj.clone()), &PropertyKey::String("b".to_string())).ok(),
                Some(Value::Number(n)) if (n - 2.0).abs() < f64::EPSILON
            ));
        }
        other => panic!("expected object, got {:?}", other),
    }

    let target = vm.get_global("target").expect("target");
    let proto = vm.get_global("proto").expect("proto");
    let target_proto = vm.get_prototype(&target);
    assert!(strict_equal(&target_proto, &proto));
    assert!(strict_equal(
        &vm.get_global("proto_result").expect("proto_result"),
        &target
    ));
    assert_eq!(
        vm.get_global("locale"),
        Some(Value::String("[object Object]".to_string()))
    );
}

#[test]
fn object_freeze_and_seal_report_state() {
    let (vm, _) = run_script(
        r#"
            var frozen_obj = { a: 1 };
            var sealed_obj = { b: 2 };
            Object.freeze(frozen_obj);
            Object.seal(sealed_obj);
            var frozen = Object.isFrozen(frozen_obj);
            var sealed = Object.isSealed(sealed_obj);
            var not_frozen = Object.isFrozen(sealed_obj);
        "#,
    );

    assert!(matches!(vm.get_global("frozen"), Some(Value::Bool(true))));
    assert!(matches!(vm.get_global("sealed"), Some(Value::Bool(true))));
    assert!(matches!(
        vm.get_global("not_frozen"),
        Some(Value::Bool(false))
    ));
}

#[test]
fn array_includes_uses_same_value_zero_and_index_of_uses_strict_equality() {
    let (vm, _) = run_script(
        r#"
            var arr = [NaN, 1];
            var includes_nan = arr.includes(NaN);
            var index_of_nan = arr.indexOf(NaN);
            globalThis.arr_len = arr.length;
            globalThis.arr_0 = arr[0];
            globalThis.arr_1 = arr[1];
        "#,
    );

    assert!(matches!(
        vm.get_global("includes_nan"),
        Some(Value::Bool(true))
    ));
    assert!(matches!(
        vm.get_global("index_of_nan"),
        Some(Value::Number(n)) if (n + 1.0).abs() < f64::EPSILON
    ));
}

#[test]
fn dense_array_property_lookup_sees_index_and_length() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let arr_data = Rc::new(RefCell::new(ArrayData::from(vec![
        Some(Value::Number(f64::NAN)),
        Some(Value::Number(1.0)),
    ])));
    let props = Rc::new(RefCell::new(ObjectData::new()));
    props.borrow_mut().properties.insert(
        PropertyKey::String("length".to_string()),
        PropertyDescriptor::data(Value::Number(2.0)),
    );
    let arr = Value::Array(arr_data, props);

    assert!(
        vm.has_property_host(&arr, &PropertyKey::String("length".to_string()))
            .unwrap()
    );
    assert!(
        vm.has_property_host(&arr, &PropertyKey::String("0".to_string()))
            .unwrap()
    );
    assert!(matches!(
        vm.get_property_host(&arr, &PropertyKey::String("0".to_string()))
            .unwrap(),
        Value::Number(n) if n.is_nan()
    ));
}

#[test]
fn array_at_returns_negative_index_and_slice_preserves_holes() {
    let (vm, _) = run_script(
        r#"
            var at_value = [10, 20, 30].at(-1);
            var sparse = [1, 2];
            delete sparse[1];
            var sliced = sparse.slice(0, 2);
        "#,
    );

    assert!(matches!(
        vm.get_global("at_value"),
        Some(Value::Number(n)) if (n - 30.0).abs() < f64::EPSILON
    ));
    match vm.get_global("sliced") {
        Some(Value::Array(arr, _)) => {
            let slots = arr.borrow();
            assert!(
                matches!(slots.get(0).cloned().flatten(), Some(Value::Number(n)) if (n - 1.0).abs() < f64::EPSILON)
            );
            assert!(slots.get(1).is_some_and(|slot| slot.is_none()));
        }
        other => panic!("expected array, got {:?}", other),
    }
}

#[test]
fn array_of_builds_array_from_arguments() {
    let (vm, _) = run_script(
        r#"
            var created = Array.of(4, 5, 6);
        "#,
    );

    match vm.get_global("created") {
        Some(Value::Array(arr, props)) => {
            assert_eq!(arr.borrow().len(), 3);
            assert!(
                matches!(arr.borrow().get(0).cloned().flatten(), Some(Value::Number(n)) if (n - 4.0).abs() < f64::EPSILON)
            );
            assert!(
                matches!(arr.borrow().get(1).cloned().flatten(), Some(Value::Number(n)) if (n - 5.0).abs() < f64::EPSILON)
            );
            assert!(
                matches!(arr.borrow().get(2).cloned().flatten(), Some(Value::Number(n)) if (n - 6.0).abs() < f64::EPSILON)
            );
            assert!(matches!(
                props.borrow().properties.get(&PropertyKey::String("length".to_string())),
                Some(PropertyDescriptor::Data {
                    value: Value::Number(n),
                    ..
                }) if (*n - 3.0).abs() < f64::EPSILON
            ));
        }
        other => panic!("expected array, got {:?}", other),
    }
}

#[test]
fn same_value_uses_reference_identity_for_objects() {
    let obj = Rc::new(RefCell::new(ObjectData::new()));
    let a = Value::Object(obj.clone());
    let b = Value::Object(obj.clone());
    let c = Value::Object(Rc::new(RefCell::new(ObjectData::new())));

    assert!(same_value(&a, &b));
    assert!(!same_value(&a, &c));
}

#[test]
fn to_string_handles_promise_values() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let promise = Value::Promise(Rc::new(RefCell::new(PromiseState::Pending {
        on_fulfilled: Vec::new(),
        on_rejected: Vec::new(),
    })));

    assert_eq!(vm.to_string(&promise).unwrap(), "[object Promise]");
}

#[test]
fn to_string_formats_integral_numbers_without_trailing_decimal() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);

    assert_eq!(vm.to_string(&Value::Number(1.0)).unwrap(), "1");
    assert_eq!(vm.to_string(&Value::Number(2.0)).unwrap(), "2");
    assert_eq!(vm.to_string(&Value::Number(-0.0)).unwrap(), "0");
}

#[test]
fn to_number_parses_radix_prefixed_strings() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);

    assert_eq!(
        vm.to_number_value(&Value::String("0x10".to_string()))
            .unwrap(),
        16.0
    );
    assert_eq!(
        vm.to_number_value(&Value::String("0b101".to_string()))
            .unwrap(),
        5.0
    );
    assert_eq!(
        vm.to_number_value(&Value::String("0o10".to_string()))
            .unwrap(),
        8.0
    );
}

#[test]
fn to_number_rejects_signed_radix_prefixed_strings() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);

    assert!(
        vm.to_number_value(&Value::String("-0x10".to_string()))
            .unwrap()
            .is_nan()
    );
    assert!(
        vm.to_number_value(&Value::String("+0b101".to_string()))
            .unwrap()
            .is_nan()
    );
}

#[test]
fn dense_array_index_beyond_max_doesnt_dense_resize() {
    let arr_data = Rc::new(RefCell::new(ArrayData::from(vec![Some(Value::Number(
        0.0,
    ))])));
    let props = Rc::new(RefCell::new(ObjectData::new()));
    props.borrow_mut().properties.insert(
        PropertyKey::String("length".to_string()),
        PropertyDescriptor::data(Value::Number(1.0)),
    );
    let arr = Value::Array(arr_data.clone(), props.clone());
    let key = PropertyKey::String("4294967295".to_string());

    set_property(&arr, &key, Value::String("x".to_string())).unwrap();

    assert_eq!(arr_data.borrow().len(), 1);
    assert!(matches!(
        get_property(&arr, &key, 0).unwrap(),
        Value::String(ref s) if s == "x"
    ));
    assert!(matches!(
        get_property(&arr, &PropertyKey::String("length".to_string()), 0).unwrap(),
        Value::Number(n) if (n - 1.0).abs() < f64::EPSILON
    ));
}

#[test]
fn sparse_array_index_updates_length_without_dense_resize() {
    let arr_data = Rc::new(RefCell::new(ArrayData::from(vec![Some(Value::Number(
        0.0,
    ))])));
    let props = Rc::new(RefCell::new(ObjectData::new()));
    props.borrow_mut().properties.insert(
        PropertyKey::String("length".to_string()),
        PropertyDescriptor::data(Value::Number(1.0)),
    );
    let arr = Value::Array(arr_data.clone(), props.clone());
    let key = PropertyKey::String("4294967294".to_string());

    set_property(&arr, &key, Value::Number(1.0)).unwrap();

    assert_eq!(arr_data.borrow().len(), 1);
    assert!(matches!(
        get_property(&arr, &key, 0).unwrap(),
        Value::Number(n) if (n - 1.0).abs() < f64::EPSILON
    ));
    assert!(matches!(
        get_property(&arr, &PropertyKey::String("length".to_string()), 0).unwrap(),
        Value::Number(n) if (n - 4294967295.0).abs() < f64::EPSILON
    ));
}

#[test]
fn dense_array_element_redefinition_preserves_configurability() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let arr_data = Rc::new(RefCell::new(ArrayData::from(vec![
        Some(Value::Number(1.0)),
        Some(Value::Number(2.0)),
    ])));
    let props = Rc::new(RefCell::new(ObjectData::new()));
    props.borrow_mut().properties.insert(
        PropertyKey::String("length".to_string()),
        PropertyDescriptor::data(Value::Number(2.0)),
    );
    let arr = Value::Array(arr_data.clone(), props.clone());
    let getter = Value::Builtin(Rc::new(|_, _| Ok(Value::Undefined)));
    let mut desc = PartialDescriptor::default();
    desc.get = Some(getter);
    desc.has_get = true;

    assert!(
        vm.define_own_property(&arr, PropertyKey::String("0".to_string()), desc)
            .unwrap()
    );
    assert!(matches!(
        props
            .borrow()
            .properties
            .get(&PropertyKey::String("0".to_string())),
        Some(PropertyDescriptor::Accessor {
            configurable: true,
            ..
        })
    ));
    assert!(
        vm.delete_property_host(&arr, &PropertyKey::String("0".to_string()))
            .unwrap()
    );
    assert!(
        !vm.has_property_host(&arr, &PropertyKey::String("0".to_string()))
            .unwrap()
    );
}

#[test]
fn reverse_with_length_shrink_clears_lower_index() {
    let (mut vm, _) = run_script(
        r#"
            var array = ["first", "second"];
            Object.defineProperty(array, 0, {
                get: function() {
                    array.length = 0;
                    return "first";
                }
            });
            array.reverse();
        "#,
    );

    match vm.get_global("array") {
        Some(Value::Array(arr, props)) => {
            assert!(
                !vm.has_property_host(
                    &Value::Array(arr.clone(), props.clone()),
                    &PropertyKey::String("0".to_string())
                )
                .unwrap()
            );
            assert!(
                vm.has_property_host(
                    &Value::Array(arr.clone(), props.clone()),
                    &PropertyKey::String("1".to_string())
                )
                .unwrap()
            );
            assert!(matches!(
                vm.get_property_host(
                    &Value::Array(arr.clone(), props.clone()),
                    &PropertyKey::String("1".to_string())
                )
                .unwrap(),
                Value::String(ref s) if s == "first"
            ));
        }
        other => panic!("expected array, got {:?}", other),
    }
}

#[test]
fn top_level_assignment_updates_global_binding() {
    let session = Parser::parse_source("var x; x = 5;").expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let result = vm.run(chunk).expect("run ok");

    assert!(matches!(result, Value::Undefined));
    assert!(matches!(vm.get_global("x"), Some(Value::Number(n)) if (n - 5.0).abs() < f64::EPSILON));
}

#[test]
fn eval_for_loop_completion_is_undefined() {
    let src = r#"
            var supreme, count;
            supreme = 5;
            var evaluated = eval("for(count=0;;) {if (count===supreme)break;else count++; }");
        "#;
    let session = Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let result = vm.run(chunk).expect("run ok");

    assert!(matches!(result, Value::Undefined));
    assert!(
        matches!(vm.get_global("supreme"), Some(Value::Number(n)) if (n - 5.0).abs() < f64::EPSILON)
    );
    assert!(
        matches!(vm.get_global("count"), Some(Value::Number(n)) if (n - 5.0).abs() < f64::EPSILON)
    );
    assert!(matches!(vm.get_global("evaluated"), Some(Value::Undefined)));
}

#[test]
fn eval_array_spread_literal_expands_values() {
    let src = r#"var evaluated = eval('[...[1, 2, 3], 4, 5]');"#;
    let session = Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let result = vm.run(chunk).expect("run ok");

    assert!(matches!(result, Value::Undefined));
    match vm.get_global("evaluated") {
        Some(Value::Array(arr, _)) => {
            let values: Vec<Value> = arr.borrow().iter().filter_map(Clone::clone).collect();
            assert_eq!(values.len(), 5);
            assert!(matches!(values[0], Value::Number(n) if (n - 1.0).abs() < f64::EPSILON));
            assert!(matches!(values[1], Value::Number(n) if (n - 2.0).abs() < f64::EPSILON));
            assert!(matches!(values[2], Value::Number(n) if (n - 3.0).abs() < f64::EPSILON));
            assert!(matches!(values[3], Value::Number(n) if (n - 4.0).abs() < f64::EPSILON));
            assert!(matches!(values[4], Value::Number(n) if (n - 5.0).abs() < f64::EPSILON));
        }
        other => panic!("expected evaluated array, got {:?}", other),
    }
}

#[test]
fn with_statement_coerces_operand_to_object() {
    let session = Parser::parse_source("with (undefined) { x = 1; }").expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let err = vm.run(chunk).expect_err("with(undefined) should throw");

    assert!(
        err.to_string()
            .contains("Cannot convert undefined or null to object")
    );
    assert!(vm.get_global("x").is_none());
}

#[test]
fn with_statement_reads_bindings_from_object_environment() {
    let session = Parser::parse_source(
        r#"
            var result;
            var obj = { value: "from-with", NaN: "shadowed" };
            with (obj) {
                result = value;
                result = result + ":" + NaN;
            }
        "#,
    )
    .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    assert_eq!(
        vm.get_global("result"),
        Some(Value::String("from-with:shadowed".into()))
    );
}

#[test]
fn with_statement_inside_constructor_reads_and_writes_object_bindings() {
    let session = Parser::parse_source(
        r#"
            var result = "result";
            var myObj = { value: "myObj_value", NaN: "obj_NaN" };
            var st_NaN = "NaN";
            var f = function() {
              with (myObj) {
                st_NaN = NaN;
                var value = "value";
                return value;
              }
            };
            var obj = new f();
        "#,
    )
    .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    assert_eq!(
        vm.get_global("st_NaN"),
        Some(Value::String("obj_NaN".into()))
    );
}

#[test]
fn with_statement_handles_common_identifier_shadowing_cases() {
    let session = Parser::parse_source(
        r#"
            this.p1 = 1;
            this.p2 = 2;
            this.p3 = 3;
            var st_NaN = "NaN";
            var st_Infinity = "Infinity";
            var st_eval = "eval";
            var st_parseInt = "parseInt";
            var st_parseFloat = "parseFloat";
            var st_isNaN = "isNaN";
            var st_isFinite = "isFinite";
            var myObj = {
              p1: "a",
              p2: "b",
              p3: "c",
              value: "myObj_value",
              valueOf: function() { return "obj_valueOf"; },
              parseInt: function() { return "obj_parseInt"; },
              NaN: "obj_NaN",
              Infinity: "obj_Infinity",
              eval: function() { return "obj_eval"; },
              parseFloat: function() { return "obj_parseFloat"; },
              isNaN: function() { return "obj_isNaN"; },
              isFinite: function() { return "obj_isFinite"; }
            };
            var f = function() {
              with (myObj) {
                st_p1 = p1;
                st_p2 = p2;
                st_p3 = p3;
                st_parseInt = parseInt;
                st_NaN = NaN;
                st_Infinity = Infinity;
                st_eval = eval;
                st_parseFloat = parseFloat;
                st_isNaN = isNaN;
                st_isFinite = isFinite;
              }
            };
            var obj = new f();
        "#,
    )
    .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    assert_eq!(
        vm.get_global("st_NaN"),
        Some(Value::String("obj_NaN".into()))
    );
    assert_eq!(
        vm.get_global("st_Infinity"),
        Some(Value::String("obj_Infinity".into()))
    );
    match vm.get_global("st_parseInt") {
        Some(Value::Closure { .. }) => {}
        other => panic!("expected function object for parseInt, got {:?}", other),
    }
    match vm.get_global("st_parseFloat") {
        Some(Value::Closure { .. }) => {}
        other => panic!("expected function object for parseFloat, got {:?}", other),
    }
}

#[test]
fn with_statement_prefers_object_bindings_over_global_nans() {
    let session = Parser::parse_source(
        r#"
            this.p1 = 1;
            this.p2 = 2;
            this.p3 = 3;
            var myObj = {
              p1: "a",
              p2: "b",
              p3: "c",
              value: "myObj_value",
              valueOf: function() { return "obj_valueOf"; },
              parseInt: function() { return "obj_parseInt"; },
              NaN: "obj_NaN",
              Infinity: "obj_Infinity",
              eval: function() { return "obj_eval"; },
              parseFloat: function() { return "obj_parseFloat"; },
              isNaN: function() { return "obj_isNaN"; },
              isFinite: function() { return "obj_isFinite"; }
            };
            var st_NaN = "NaN";
            var f = function() {
              with (myObj) {
                st_NaN = NaN;
                st_Infinity = Infinity;
                st_eval = eval;
                st_parseFloat = parseFloat;
                st_isNaN = isNaN;
                st_isFinite = isFinite;
                p1 = "x1";
                this.p2 = "x2";
                delete p3;
                var value = "value";
              }
            };
            var st_Infinity = "Infinity";
            var st_eval = "eval";
            var st_parseFloat = "parseFloat";
            var st_isNaN = "isNaN";
            var st_isFinite = "isFinite";
            f();
        "#,
    )
    .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    assert_eq!(
        vm.get_global("st_NaN"),
        Some(Value::String("obj_NaN".into()))
    );
    assert_eq!(
        vm.get_global("st_Infinity"),
        Some(Value::String("obj_Infinity".into()))
    );
    match vm.get_global("st_eval") {
        Some(Value::Closure { .. }) => {}
        other => panic!("expected function object for eval, got {:?}", other),
    }
    match vm.get_global("st_parseFloat") {
        Some(Value::Closure { .. }) => {}
        other => panic!("expected function object for parseFloat, got {:?}", other),
    }
    match vm.get_global("st_isNaN") {
        Some(Value::Closure { .. }) => {}
        other => panic!("expected function object for isNaN, got {:?}", other),
    }
    match vm.get_global("st_isFinite") {
        Some(Value::Closure { .. }) => {}
        other => panic!("expected function object for isFinite, got {:?}", other),
    }
}

#[test]
fn with_statement_prefers_object_bindings_over_global_nans_in_plain_call() {
    let session = Parser::parse_source(
        r#"
            this.p1 = 1;
            this.p2 = 2;
            this.p3 = 3;
            var result = "result";
            var myObj = {p1: "a",
                         p2: "b",
                         p3: "c",
                         value: "myObj_value",
                         valueOf: function(){return "obj_valueOf";},
                         parseInt: function(){return "obj_parseInt";},
                         NaN: "obj_NaN",
                         Infinity: "obj_Infinity",
                         eval: function(){return "obj_eval";},
                         parseFloat: function(){return "obj_parseFloat";},
                         isNaN: function(){return "obj_isNaN";},
                         isFinite: function(){return "obj_isFinite";}
            };
            var del;
            var st_p1 = "p1";
            var st_p2 = "p2";
            var st_p3 = "p3";
            var st_parseInt = "parseInt";
            var st_NaN = "NaN";
            var st_Infinity = "Infinity";
            var st_eval = "eval";
            var st_parseFloat = "parseFloat";
            var st_isNaN = "isNaN";
            var st_isFinite = "isFinite";

            var f = function(){
              with(myObj){
                st_p1 = p1;
                st_p2 = p2;
                st_p3 = p3;
                st_parseInt = parseInt;
                st_NaN = NaN;
                st_Infinity = Infinity;
                st_eval = eval;
                st_parseFloat = parseFloat;
                st_isNaN = isNaN;
                st_isFinite = isFinite;
                p1 = "x1";
                this.p2 = "x2";
                del = delete p3;
                var p4 = "x4";
                p5 = "x5";
                var value = "value";
              }
            };
            f();
        "#,
    )
    .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    assert_eq!(
        vm.get_global("st_NaN"),
        Some(Value::String("obj_NaN".into()))
    );
}

#[test]
fn with_statement_prefers_object_bindings_over_global_nans_after_harness() {
    typecheck::set_strict_override(Some(false));
    struct ResetStrict;
    impl Drop for ResetStrict {
        fn drop(&mut self) {
            typecheck::set_strict_override(None);
        }
    }
    let _reset_strict = ResetStrict;

    let globals = test_globals();
    globals.borrow_mut().properties.insert(
        PropertyKey::String("NaN".to_string()),
        PropertyDescriptor::data(Value::Number(f64::NAN)),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("Infinity".to_string()),
        PropertyDescriptor::data(Value::Number(f64::INFINITY)),
    );
    let mut vm = Vm::new(globals);

    for harness_name in ["assert.js", "sta.js"] {
        let path = std::path::Path::new("test262/harness").join(harness_name);
        let src = std::fs::read_to_string(&path).expect("read harness");
        let session = Parser::parse_source(&src).expect("parse harness");
        let program = session.program();
        let chunk = compiler::compile(program, session.source()).expect("compile harness");
        vm.run(chunk).expect("run harness");
    }

    let session = Parser::parse_source(
        r#"
            this.p1 = 1;
            this.p2 = 2;
            this.p3 = 3;
            var result = "result";
            var myObj = {p1: "a",
                         p2: "b",
                         p3: "c",
                         value: "myObj_value",
                         valueOf: function(){return "obj_valueOf";},
                         parseInt: function(){return "obj_parseInt";},
                         NaN: "obj_NaN",
                         Infinity: "obj_Infinity",
                         eval: function(){return "obj_eval";},
                         parseFloat: function(){return "obj_parseFloat";},
                         isNaN: function(){return "obj_isNaN";},
                         isFinite: function(){return "obj_isFinite";}
            };
            var del;
            var st_p1 = "p1";
            var st_p2 = "p2";
            var st_p3 = "p3";
            var st_parseInt = "parseInt";
            var st_NaN = "NaN";
            var st_Infinity = "Infinity";
            var st_eval = "eval";
            var st_parseFloat = "parseFloat";
            var st_isNaN = "isNaN";
            var st_isFinite = "isFinite";

            var f = function(){
              with(myObj){
                st_p1 = p1;
                st_p2 = p2;
                st_p3 = p3;
                st_parseInt = parseInt;
                st_NaN = NaN;
                st_Infinity = Infinity;
                st_eval = eval;
                st_parseFloat = parseFloat;
                st_isNaN = isNaN;
                st_isFinite = isFinite;
                p1 = "x1";
                this.p2 = "x2";
                del = delete p3;
                var p4 = "x4";
                p5 = "x5";
                var value = "value";
              }
            };
            f();
        "#,
    )
    .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    vm.run(chunk).expect("run ok");

    assert_eq!(
        vm.get_global("st_NaN"),
        Some(Value::String("obj_NaN".into()))
    );
}

#[test]
fn with_statement_var_initializer_writes_through_object_binding() {
    let session = Parser::parse_source(
        r#"
            var result;
            var myObj = { value: "myObj_value" };
            with (myObj) {
              var value = "value";
            }
            result = value;
        "#,
    )
    .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    assert_eq!(vm.get_global("result"), Some(Value::Undefined));
    match vm.get_global("myObj") {
        Some(Value::Object(obj)) => {
            let value_key = PropertyKey::String("value".to_string());
            let obj_borrow = obj.borrow();
            match obj_borrow.properties.get(&value_key) {
                Some(PropertyDescriptor::Data { value, .. }) => {
                    assert_eq!(value, &Value::String("value".into()))
                }
                other => panic!("expected data property for myObj.value, got {:?}", other),
            }
        }
        other => panic!("expected object for myObj, got {:?}", other),
    }
}

#[test]
fn strict_assignment_to_non_extensible_object_throws() {
    let src = r#"
            function f() {
                'use strict';
                var _8_7_2_5 = {};
                Object.preventExtensions(_8_7_2_5);
                _8_7_2_5.b = 11;
            }
            f();
        "#;
    let session = Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let err = vm.run(chunk).expect_err("strict assignment should throw");

    assert!(
        err.to_string()
            .contains("Cannot add property to non-extensible object")
    );
    assert!(vm.get_global("b").is_none());
}

#[test]
fn mapped_arguments_object_exposes_callee() {
    let session = Parser::parse_source(
        r#"
            function testcase() {
                var desc = Object.getOwnPropertyDescriptor(arguments, "callee");
                return typeof arguments.callee === "function"
                    && desc.configurable === true
                    && desc.enumerable === false
                    && desc.writable === true;
            }
            var result = testcase();
        "#,
    )
    .expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");
    assert!(matches!(vm.get_global("result"), Some(Value::Bool(true))));
}

#[test]
fn strict_arguments_object_poison_pill_throws_on_callee() {
    let session = Parser::parse_source(
        r#"
            function testcase() {
                'use strict';
                return arguments.callee;
            }
            testcase();
        "#,
    )
    .expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let err = vm
        .run(chunk)
        .expect_err("strict callee access should throw");
    assert!(
        err.to_string()
            .contains("strict arguments callee/caller is forbidden")
    );
}

#[test]
fn mapped_arguments_object_prototype_and_aliasing() {
    let session = Parser::parse_source(
        r#"
            function testcase(a) {
                arguments[0] = 2;
                a = 3;
                return Object.getPrototypeOf(arguments) === Object.prototype
                    && arguments[0] === 3
                    && a === 3
                    && Object.getOwnPropertyDescriptor(arguments, "length").enumerable === false;
            }
            var result = testcase(1);
        "#,
    )
    .expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");
    assert!(matches!(vm.get_global("result"), Some(Value::Bool(true))));
}

#[test]
fn arguments_object_is_iterable_with_array_iterator() {
    let session = Parser::parse_source(
        r#"
            function mapped(a, b, c) {
                return arguments[Symbol.iterator].name
                    + ":" + [...arguments].join(",");
            }
            function unmapped([a], b, c) {
                return arguments[Symbol.iterator].name
                    + ":" + [...arguments].join(",");
            }
            var mapped_result = mapped(1, 2, 3);
            var unmapped_result = unmapped([1], 2, 3);
        "#,
    )
    .expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    assert!(matches!(
        vm.get_global("mapped_result"),
        Some(Value::String(s)) if s == "values:1,2,3"
    ));
    assert!(matches!(
        vm.get_global("unmapped_result"),
        Some(Value::String(s)) if s == "values:1,2,3"
    ));
}

#[test]
fn void_operator_evaluates_to_undefined() {
    let session = Parser::parse_source("var x = void 0;").expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let result = vm.run(chunk).expect("run ok");

    assert!(matches!(result, Value::Undefined));
    assert!(matches!(vm.get_global("x"), Some(Value::Undefined)));
}

#[test]
fn logical_and_short_circuits_and_preserves_left_operand() {
    let session = Parser::parse_source("var x = 0 && bump();").expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let calls = Rc::new(RefCell::new(0usize));
    let globals = test_globals();
    globals.borrow_mut().properties.insert(
        PropertyKey::String("bump".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new({
            let calls = calls.clone();
            move |_vm, _args| {
                *calls.borrow_mut() += 1;
                Ok(Value::Number(99.0))
            }
        }))),
    );

    let mut vm = Vm::new(globals);
    let result = vm.run(chunk).expect("run ok");

    assert_eq!(*calls.borrow(), 0);
    assert!(matches!(result, Value::Undefined));
    assert!(matches!(vm.get_global("x"), Some(Value::Number(n)) if n == 0.0));
}

#[test]
fn logical_or_short_circuits_and_preserves_left_operand() {
    let session = Parser::parse_source("var x = 1 || bump();").expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");

    let calls = Rc::new(RefCell::new(0usize));
    let globals = test_globals();
    globals.borrow_mut().properties.insert(
        PropertyKey::String("bump".to_string()),
        PropertyDescriptor::data(Value::Builtin(Rc::new({
            let calls = calls.clone();
            move |_vm, _args| {
                *calls.borrow_mut() += 1;
                Ok(Value::Number(99.0))
            }
        }))),
    );

    let mut vm = Vm::new(globals);
    let result = vm.run(chunk).expect("run ok");

    assert_eq!(*calls.borrow(), 0);
    assert!(matches!(result, Value::Undefined));
    assert!(matches!(vm.get_global("x"), Some(Value::Number(n)) if n == 1.0));
}

#[test]
fn lexical_binding_reads_before_init_throw_reference_error() {
    let session = Parser::parse_source("function f(){ x; let x = 1; } f();").expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let err = vm.run(chunk).expect_err("run should fail for TDZ read");
    let msg = err.to_string();
    assert!(
        msg.contains("Cannot access lexical binding before initialization"),
        "unexpected error: {msg}"
    );
}

#[test]
fn captured_lexical_binding_reads_before_init_throw_reference_error() {
    let session = Parser::parse_source(
        "function g(){ function f(){ return x + 1; } return f(); let x = 1; } g();",
    )
    .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let err = vm
        .run(chunk)
        .expect_err("run should fail for captured TDZ read");
    let msg = err.to_string();
    assert!(
        msg.contains("Cannot access lexical binding before initialization"),
        "unexpected error: {msg}"
    );
}

#[test]
fn try_block_lexical_shadow_does_not_escape_to_catch() {
    let session = Parser::parse_source(
            "var out = (function(x){ try { let x = 'inner'; throw 0; } catch(e) { return x; } })('outer');",
        )
        .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");
    assert!(matches!(vm.get_global("out"), Some(Value::String(s)) if s == "outer"));
}

#[test]
fn for_in_lexical_declaration_with_initializer_is_syntax_error() {
    let session =
        Parser::parse_source("for (let x = 3 in {}) {}").expect("parser currently accepts syntax");
    let program = session.program();
    let err = compiler::compile(program, session.source())
        .expect_err("compile should reject lexical initializer in for-in");
    assert!(err.to_string().contains("SyntaxError"));
}

#[test]
fn var_binding_reads_before_init_return_undefined() {
    let session = Parser::parse_source("function f(){ return x; var x = 1; } var out = f();")
        .expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");
    assert!(matches!(vm.get_global("out"), Some(Value::Undefined)));
}

#[test]
fn generator_next_yields_values_and_done() {
    let src = r#"
            function* g() { yield 1; yield 2; return 3; }
            var it = g();
            var a = it.next();
            var b = it.next();
            var c = it.next();
            var d = it.next();
        "#;
    let session = Parser::parse_source(src).expect("parse ok");
    let program = session.program();
    let chunk = compiler::compile(program, session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    let a = vm.get_global("a").expect("a exists");
    let b = vm.get_global("b").expect("b exists");
    let c = vm.get_global("c").expect("c exists");
    let d = vm.get_global("d").expect("d exists");

    assert!(matches!(
        get_property(&a, &PropertyKey::String("value".to_string()), 0).unwrap(),
        Value::Number(n) if n == 1.0
    ));
    assert!(matches!(
        get_property(&a, &PropertyKey::String("done".to_string()), 0).unwrap(),
        Value::Bool(false)
    ));
    assert!(matches!(
        get_property(&b, &PropertyKey::String("value".to_string()), 0).unwrap(),
        Value::Number(n) if n == 2.0
    ));
    assert!(matches!(
        get_property(&c, &PropertyKey::String("value".to_string()), 0).unwrap(),
        Value::Number(n) if n == 3.0
    ));
    assert!(matches!(
        get_property(&c, &PropertyKey::String("done".to_string()), 0).unwrap(),
        Value::Bool(true)
    ));
    assert!(matches!(
        get_property(&d, &PropertyKey::String("done".to_string()), 0).unwrap(),
        Value::Bool(true)
    ));
}

#[test]
fn harness_property_helper_works_across_multiple_runs() {
    let globals = test_globals();
    globals.borrow_mut().properties.insert(
        PropertyKey::String("NaN".to_string()),
        PropertyDescriptor::data(Value::Number(f64::NAN)),
    );
    globals.borrow_mut().properties.insert(
        PropertyKey::String("Infinity".to_string()),
        PropertyDescriptor::data(Value::Number(f64::INFINITY)),
    );
    runtime_utils::install_minimal_kernel_constructors(&globals);
    let mut vm = Vm::new(globals);

    for harness_name in ["assert.js", "sta.js", "propertyHelper.js"] {
        let path = std::path::Path::new("test262/harness").join(harness_name);
        let src = std::fs::read_to_string(&path).expect("read harness");
        let session = Parser::parse_source(&src).expect("parse harness");
        let program = session.program();
        let chunk = compiler::compile_oxc(program, session.source()).expect("compile harness");
        vm.run(chunk).expect("run harness");
    }

    typecheck::set_strict_override(Some(true));
    let src = r#"
            var obj;
            var prop = 'prop';

            function reset(desc) {
              desc.value = prop;
              obj = Object.defineProperty({}, prop, desc);
            }

            function checkDesc(desc) {
              reset(desc);
              assert(verifyProperty(obj, prop, desc));
            }

            checkDesc({ enumerable: false, writable: false, configurable: false });
        "#;
    let session = Parser::parse_source(src).expect("parse test");
    let program = session.program();
    let chunk = compiler::compile_oxc(program, session.source()).expect("compile test");
    vm.run(chunk).expect("run test");
    typecheck::set_strict_override(None);
}

#[test]
fn class_declaration_binds_constructor_function() {
    let src = r#"
            class A {}
        "#;
    let session = Parser::parse_source(src).expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");
    assert!(matches!(vm.get_global("A"), Some(Value::Closure { .. })));
}

#[test]
fn super_property_access_uses_home_object_and_lexical_this() {
    let src = r#"
            class C {
                methodThis() { return this; }
                method() { return super.toString(); }
                arrow() { return (() => super.toString())(); }
                arrowThis() { return (() => this)() === this; }
                assign(v) { super.value = v; return this.value; }
            }
            var c = new C();
            var method_this = c.methodThis();
            var method_result = c.method();
            var arrow_result = c.arrow();
            var arrow_this_matches = c.arrowThis();
            var assign_result = c.assign(9);
        "#;
    let session = Parser::parse_source(src).expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    let method_result = vm.get_global("method_result");
    let method_this = vm.get_global("method_this");
    let arrow_result = vm.get_global("arrow_result");
    let arrow_this_matches = vm.get_global("arrow_this_matches");
    let assign_result = vm.get_global("assign_result");
    assert!(
        matches!(method_this, Some(Value::Object(_))),
        "unexpected method_this: {:?}",
        method_this
    );
    assert!(
        matches!(method_result, Some(Value::String(ref s)) if s == "[object Object]"),
        "unexpected method_result: {:?}",
        method_result
    );
    assert!(
        matches!(arrow_result, Some(Value::String(ref s)) if s == "[object Object]"),
        "unexpected arrow_result: {:?}",
        arrow_result
    );
    assert!(
        matches!(arrow_this_matches, Some(Value::Bool(true))),
        "unexpected arrow_this_matches: {:?}",
        arrow_this_matches
    );
    assert!(
        matches!(assign_result, Some(Value::Number(n)) if (n - 9.0).abs() < f64::EPSILON),
        "unexpected assign_result: {:?}",
        assign_result
    );
}

#[test]
fn microtasks_run_before_dynamic_import_jobs() {
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let order = Rc::new(RefCell::new(Vec::<&'static str>::new()));

    vm.queue_microtask({
        let order = order.clone();
        move |_vm| {
            order.borrow_mut().push("microtask");
            Ok(())
        }
    });
    vm.queue_dynamic_import_job({
        let order = order.clone();
        move |_vm| {
            order.borrow_mut().push("dynamic-import");
            Ok(())
        }
    });

    vm.flush_microtasks().expect("flush ok");
    assert_eq!(&*order.borrow(), &["microtask", "dynamic-import"]);
}

#[test]
fn array_reduce_without_initial_value_throws_on_empty_array() {
    let src = r#"[].reduce(function(acc, value) { return acc + value; });"#;
    let session = Parser::parse_source(src).expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    let err = vm.run(chunk).expect_err("reduce should throw");
    assert!(
        err.to_string()
            .contains("Reduce of empty array with no initial value")
    );
}

#[test]
fn object_keys_orders_indices_before_strings() {
    let src = r#"
            var o = {};
            o.b = 1;
            o[2] = 2;
            o.a = 3;
            o[1] = 4;
            var keys = Object.keys(o);
        "#;
    let session = Parser::parse_source(src).expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");
    match vm.get_global("keys") {
        Some(Value::Array(arr, _)) => {
            let values: Vec<String> = arr
                .borrow()
                .iter()
                .filter_map(|v| match v {
                    Some(Value::String(s)) => Some(s.clone()),
                    _ => None,
                })
                .collect();
            assert_eq!(values, vec!["1", "2", "b", "a"]);
        }
        other => panic!("expected keys array, got {:?}", other),
    }
}

#[test]
fn for_in_keys_walks_prototype_chain_without_duplicates() {
    let globals = test_globals();
    let vm = Vm::new(globals.clone());

    let mut proto = ObjectData::new();
    proto.properties.insert(
        PropertyKey::String("b".to_string()),
        PropertyDescriptor::data(Value::Number(2.0)),
    );
    proto.properties.insert(
        PropertyKey::String("1".to_string()),
        PropertyDescriptor::data(Value::Number(1.0)),
    );
    let proto = Rc::new(RefCell::new(proto));

    let mut obj = ObjectData::with_prototype(Value::Object(proto.clone()));
    obj.properties.insert(
        PropertyKey::String("a".to_string()),
        PropertyDescriptor::data(Value::Number(3.0)),
    );
    obj.properties.insert(
        PropertyKey::String("1".to_string()),
        PropertyDescriptor::data(Value::Number(4.0)),
    );
    let obj = Value::Object(Rc::new(RefCell::new(obj)));

    let keys = vm.for_in_keys_of_value(&obj).expect("keys ok");
    assert_eq!(keys, vec!["1", "a", "b"]);
}

#[test]
fn global_this_matches_ecmascript_public_surface() {
    let globals = test_globals();
    let vm = Vm::new(globals.clone());

    let mut actual = vm
        .list_own_keys_of_value(&Value::Object(globals))
        .expect("globalThis keys");
    actual.sort();

    let mut expected = vec![
        "Infinity",
        "NaN",
        "undefined",
        "eval",
        "isFinite",
        "isNaN",
        "parseFloat",
        "parseInt",
        "encodeURI",
        "encodeURIComponent",
        "decodeURI",
        "decodeURIComponent",
        "Object",
        "Function",
        "Boolean",
        "Symbol",
        "Error",
        "AggregateError",
        "EvalError",
        "RangeError",
        "ReferenceError",
        "SyntaxError",
        "TypeError",
        "URIError",
        "Number",
        "BigInt",
        "Math",
        "Date",
        "String",
        "RegExp",
        "Array",
        "Int8Array",
        "Uint8Array",
        "Uint8ClampedArray",
        "Int16Array",
        "Uint16Array",
        "Int32Array",
        "Uint32Array",
        "Float32Array",
        "Float64Array",
        "BigInt64Array",
        "BigUint64Array",
        "Map",
        "Set",
        "WeakMap",
        "WeakSet",
        "WeakRef",
        "FinalizationRegistry",
        "ShadowRealm",
        "Temporal",
        "ArrayBuffer",
        "SharedArrayBuffer",
        "DataView",
        "Atomics",
        "Promise",
        "Reflect",
        "Proxy",
        "JSON",
        "globalThis",
    ]
    .into_iter()
    .map(String::from)
    .collect::<Vec<_>>();
    expected.sort();

    assert_eq!(actual, expected);
}

#[test]
fn json_stringify_preserves_descriptor_fields() {
    let src = r#"
            var desc = Object.getOwnPropertyDescriptor(globalThis, "globalThis");
            Object.defineProperty(globalThis, "keys", {
                value: Object.keys(desc).join(","),
                writable: true,
                enumerable: false,
                configurable: true,
            });
            Object.defineProperty(globalThis, "enumerableValue", {
                value: String(desc.enumerable),
                writable: true,
                enumerable: false,
                configurable: true,
            });
            Object.defineProperty(globalThis, "text", {
                value: JSON.stringify(Object.defineProperties({}, {
                    configurable: { value: true, writable: true, enumerable: true },
                    enumerable: { value: false, writable: true, enumerable: true },
                    writable: { value: true, writable: true, enumerable: true },
                    value: { value: {}, writable: true, enumerable: true },
                })),
                writable: true,
                enumerable: false,
                configurable: true,
            });
        "#;
    let session = Parser::parse_source(src).expect("parse ok");
    let chunk = compiler::compile(session.program(), session.source()).expect("compile ok");
    let globals = test_globals();
    let mut vm = Vm::new(globals);
    vm.run(chunk).expect("run ok");

    match vm.get_global("keys") {
        Some(Value::String(s)) => {
            assert_eq!(s, "value,writable,enumerable,configurable");
        }
        other => panic!("expected descriptor keys string, got {:?}", other),
    }
    match vm.get_global("enumerableValue") {
        Some(Value::String(s)) => {
            assert_eq!(s, "false");
        }
        other => panic!("expected descriptor enumerable value, got {:?}", other),
    }
    match vm.get_global("text") {
        Some(Value::String(s)) => {
            assert_eq!(
                s,
                r#"{"configurable":true,"enumerable":false,"writable":true,"value":{}}"#
            );
        }
        other => panic!("expected serialized descriptor string, got {:?}", other),
    }
}

#[test]
fn vm_new_populates_intrinsics_table() {
    let globals = test_globals();
    let vm = Vm::new(globals);
    let intrinsics = &vm.realm().intrinsics;
    assert!(intrinsics.object_constructor.is_some());
    assert!(intrinsics.throw_type_error.is_some());
    assert!(intrinsics.array_iterator_prototype.is_some());
    assert!(!intrinsics.typed_array_constructors.is_empty());
}
