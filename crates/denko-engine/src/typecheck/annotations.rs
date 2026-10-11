// SPDX-License-Identifier: MPL-2.0

use super::*;

pub(super) fn convert_type_ann(ty: &oast::TSTypeAnnotation<'_>) -> Result<Type, TypeCheckerError> {
  convert_ts_type(&ty.type_annotation)
}

pub(super) fn convert_ts_type(ty: &oast::TSType<'_>) -> Result<Type, TypeCheckerError> {
  use oast::TSType::*;

  match ty {
    TSNeverKeyword(_) => Ok(Type::Never),
    TSNumberKeyword(_) => Ok(Type::Number),
    TSStringKeyword(_) => Ok(Type::String),
    TSBooleanKeyword(_) => Ok(Type::Boolean),
    TSNullKeyword(_) => Ok(Type::Null),
    TSUndefinedKeyword(_) => Ok(Type::Undefined),
    TSVoidKeyword(_) => Ok(Type::Void),
    TSAnyKeyword(_) => Ok(Type::Any),
    TSUnknownKeyword(_) => Ok(Type::Unknown),
    TSObjectKeyword(_) => Ok(Type::Object(HashMap::new())),
    TSArrayType(t) => Ok(Type::Array(Box::new(convert_ts_type(&t.element_type)?))),
    TSParenthesizedType(t) => convert_ts_type(&t.type_annotation),
    TSTypeReference(reference) => match &reference.type_name {
      oast::TSTypeName::IdentifierReference(identifier) => Ok(Type::TypeReference {
        name: identifier.name.to_string(),
        arguments: reference
          .type_arguments
          .as_ref()
          .map(|arguments| {
            arguments
              .params
              .iter()
              .map(convert_ts_type)
              .collect::<Result<Vec<_>, _>>()
          })
          .transpose()?
          .unwrap_or_default(),
      }),
      oast::TSTypeName::QualifiedName(qualified) => Ok(Type::TypeReference {
        name: qualified_type_name(&qualified.left, qualified.right.name.as_str()),
        arguments: reference
          .type_arguments
          .as_ref()
          .map(|arguments| {
            arguments
              .params
              .iter()
              .map(convert_ts_type)
              .collect::<Result<Vec<_>, _>>()
          })
          .transpose()?
          .unwrap_or_default(),
      }),
      oast::TSTypeName::ThisExpression(_) => Ok(Type::TypeReference {
        name: "this".to_string(),
        arguments: Vec::new(),
      }),
    },
    TSConditionalType(conditional) => Ok(Type::Conditional {
      distributive_parameter: naked_type_parameter_name(&conditional.check_type),
      check: Box::new(convert_ts_type(&conditional.check_type)?),
      extends: Box::new(convert_ts_type(&conditional.extends_type)?),
      true_type: Box::new(convert_ts_type(&conditional.true_type)?),
      false_type: Box::new(convert_ts_type(&conditional.false_type)?),
    }),
    TSIntersectionType(intersection) => Ok(Type::Intersection(
      intersection
        .types
        .iter()
        .map(convert_ts_type)
        .collect::<Result<Vec<_>, _>>()?,
    )),
    TSLiteralType(literal) => convert_literal_type(&literal.literal),
    TSTupleType(tuple) => Ok(Type::Tuple(
      tuple
        .element_types
        .iter()
        .map(convert_tuple_element)
        .collect::<Result<Vec<_>, _>>()?,
    )),
    TSTypeOperatorType(operator) => {
      let operand = convert_ts_type(&operator.type_annotation)?;
      if operator.operator == oast::TSTypeOperatorOperator::Keyof {
        Ok(Type::KeyOf(Box::new(operand)))
      } else {
        Ok(operand)
      }
    }
    TSIndexedAccessType(access) => Ok(Type::IndexedAccess {
      object: Box::new(convert_ts_type(&access.object_type)?),
      index: Box::new(convert_ts_type(&access.index_type)?),
    }),
    TSInferType(infer) => Ok(Type::Infer(infer.type_parameter.name.name.to_string())),
    TSMappedType(mapped) => Ok(Type::Mapped {
      parameter: mapped.key.name.to_string(),
      constraint: Box::new(convert_ts_type(&mapped.constraint)?),
      value: Box::new(
        mapped
          .type_annotation
          .as_ref()
          .map(convert_ts_type)
          .transpose()?
          .unwrap_or(Type::Unknown),
      ),
    }),
    TSTemplateLiteralType(template) => Ok(Type::TemplateLiteral {
      quasis: template
        .quasis
        .iter()
        .map(|quasi| {
          quasi
            .value
            .cooked
            .as_ref()
            .unwrap_or(&quasi.value.raw)
            .to_string()
        })
        .collect(),
      substitutions: template
        .types
        .iter()
        .map(convert_ts_type)
        .collect::<Result<Vec<_>, _>>()?,
    }),
    TSTypeLiteral(literal) => signatures_to_object(&literal.members),
    TSFunctionType(function) => Ok(Type::Function(
      parameter_types(&function.params)?,
      Box::new(convert_type_ann(&function.return_type)?),
    )),
    TSUnionType(u) => {
      let mut variants = Vec::new();
      for member in &u.types {
        variants.push(convert_ts_type(member)?);
      }
      Ok(Type::Union(variants))
    }
    _ => Ok(Type::Unknown),
  }
}

pub(super) fn type_definition(
  parameters: Option<&oast::TSTypeParameterDeclaration<'_>>,
  body: &oast::TSType<'_>,
) -> Result<TypeDefinition, TypeCheckerError> {
  Ok(TypeDefinition {
    parameters: convert_type_parameters(parameters)?,
    body: convert_ts_type(body)?,
  })
}

pub(super) fn convert_type_parameters(
  declaration: Option<&oast::TSTypeParameterDeclaration<'_>>
) -> Result<Vec<TypeParameter>, TypeCheckerError> {
  declaration.map_or_else(
    || Ok(Vec::new()),
    |declaration| {
      declaration
        .params
        .iter()
        .map(|parameter| {
          Ok(TypeParameter {
            name: parameter.name.name.to_string(),
            constraint: parameter
              .constraint
              .as_ref()
              .map(convert_ts_type)
              .transpose()?,
            default: parameter
              .default
              .as_ref()
              .map(convert_ts_type)
              .transpose()?,
            variance: match (parameter.r#in, parameter.out) {
              (false, false) => Variance::Unspecified,
              (true, false) => Variance::In,
              (false, true) => Variance::Out,
              (true, true) => Variance::InOut,
            },
          })
        })
        .collect()
    },
  )
}

pub(super) fn naked_type_parameter_name(typ: &oast::TSType<'_>) -> Option<String> {
  if let oast::TSType::TSTypeReference(reference) = typ
    && reference.type_arguments.is_none()
    && let oast::TSTypeName::IdentifierReference(identifier) = &reference.type_name
  {
    return Some(identifier.name.to_string());
  }
  None
}

pub(super) fn convert_literal_type(
  literal: &oast::TSLiteral<'_>
) -> Result<Type, TypeCheckerError> {
  match literal {
    oast::TSLiteral::BooleanLiteral(literal) => {
      Ok(Type::Literal(LiteralType::Boolean(literal.value)))
    }
    oast::TSLiteral::NumericLiteral(literal) => {
      Ok(Type::Literal(LiteralType::Number(literal.value.to_bits())))
    }
    oast::TSLiteral::StringLiteral(literal) => Ok(Type::Literal(LiteralType::String(
      literal.value.to_string(),
    ))),
    _ => Ok(Type::Unknown),
  }
}

pub(super) fn convert_tuple_element(
  element: &oast::TSTupleElement<'_>
) -> Result<Type, TypeCheckerError> {
  match element {
    oast::TSTupleElement::TSOptionalType(optional) => Ok(normalize_union(vec![
      convert_ts_type(&optional.type_annotation)?,
      Type::Undefined,
    ])),
    oast::TSTupleElement::TSRestType(rest) => Ok(Type::Rest(Box::new(convert_ts_type(
      &rest.type_annotation,
    )?))),
    oast::TSTupleElement::TSNamedTupleMember(named) => {
      let typ = convert_tuple_element(&named.element_type)?;
      if named.optional {
        Ok(normalize_union(vec![typ, Type::Undefined]))
      } else {
        Ok(typ)
      }
    }
    _ => element
      .as_ts_type()
      .map_or_else(|| Ok(Type::Unknown), convert_ts_type),
  }
}
