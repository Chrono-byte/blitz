use super::*;
use crate::gc;

pub(crate) fn builtin_object(f: BuiltinFn, function_proto: &Rc<RefCell<ObjectData>>) -> Value {
    let props = gc::track_object(Rc::new(RefCell::new(ObjectData::new())));
    props
        .borrow_mut()
        .set_prototype_value(Value::Object(Rc::clone(function_proto)));
    Value::BuiltinObject(f, props)
}

pub(crate) fn set_builtin_length(target: &Value, length: f64) {
    let desc = PropertyDescriptor::Data {
        value: Value::Number(length),
        writable: false,
        enumerable: false,
        configurable: true,
    };
    match target {
        Value::Object(props)
        | Value::Array(_, props)
        | Value::Closure { props, .. }
        | Value::BuiltinObject(_, props) => {
            props
                .borrow_mut()
                .properties
                .insert(PropertyKey::String("length".to_string()), desc);
        }
        _ => {}
    }
}

pub(crate) fn set_builtin_name(target: &Value, name: &str) {
    let desc = PropertyDescriptor::Data {
        value: Value::String(name.to_string()),
        writable: false,
        enumerable: false,
        configurable: true,
    };
    match target {
        Value::Object(props)
        | Value::Array(_, props)
        | Value::Closure { props, .. }
        | Value::BuiltinObject(_, props) => {
            props
                .borrow_mut()
                .properties
                .insert(PropertyKey::String("name".to_string()), desc);
        }
        _ => {}
    }
}
