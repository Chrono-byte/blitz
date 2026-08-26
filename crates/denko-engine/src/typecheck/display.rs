// SPDX-License-Identifier: MPL-2.0

use super::{LiteralType, Type};

pub fn type_to_string(t: &Type) -> String {
  match t {
    Type::Never => "never".to_string(),
    Type::Number => "number".to_string(),
    Type::String => "string".to_string(),
    Type::Boolean => "boolean".to_string(),
    Type::Null => "null".to_string(),
    Type::Undefined => "undefined".to_string(),
    Type::Void => "void".to_string(),
    Type::Any => "any".to_string(),
    Type::Unknown => "unknown".to_string(),
    Type::Array(inner) => format!("{}[]", type_to_string(inner)),
    Type::Tuple(elements) => format!(
      "[{}]",
      elements
        .iter()
        .map(type_to_string)
        .collect::<Vec<_>>()
        .join(", ")
    ),
    Type::Rest(element) => format!("...{}", type_to_string(element)),
    Type::Object(properties) => {
      let mut properties = properties.iter().collect::<Vec<_>>();
      properties.sort_by_key(|(name, _)| name.as_str());
      format!(
        "{{ {} }}",
        properties
          .into_iter()
          .map(|(name, typ)| format!("{name}: {}", type_to_string(typ)))
          .collect::<Vec<_>>()
          .join("; ")
      )
    }
    Type::Function(params, ret) => format!(
      "({}) => {}",
      params
        .iter()
        .enumerate()
        .map(|(index, typ)| format!("arg{index}: {}", type_to_string(typ)))
        .collect::<Vec<_>>()
        .join(", "),
      type_to_string(ret)
    ),
    Type::TypeReference { name, arguments } => {
      if arguments.is_empty() {
        name.clone()
      } else {
        format!(
          "{}<{}>",
          name,
          arguments
            .iter()
            .map(type_to_string)
            .collect::<Vec<_>>()
            .join(", ")
        )
      }
    }
    Type::Union(v) => v
      .iter()
      .map(type_to_string)
      .collect::<Vec<_>>()
      .join(" | "),
    Type::Intersection(v) => v
      .iter()
      .map(type_to_string)
      .collect::<Vec<_>>()
      .join(" & "),
    Type::Literal(LiteralType::String(value)) => format!("{:?}", value),
    Type::Literal(LiteralType::Number(bits)) => f64::from_bits(*bits).to_string(),
    Type::Literal(LiteralType::Boolean(value)) => value.to_string(),
    Type::Conditional { .. } => "conditional type".to_string(),
    Type::KeyOf(object) => format!("keyof {}", type_to_string(object)),
    Type::IndexedAccess { object, index } => {
      format!("{}[{}]", type_to_string(object), type_to_string(index))
    }
    Type::Infer(name) => format!("infer {}", name),
    Type::Mapped { parameter, .. } => format!("{{ [{} in ...]: ... }}", parameter),
    Type::TemplateLiteral { .. } => "template literal type".to_string(),
    Type::Builtin(b) => format!("builtin::{:?}", b),
  }
}
