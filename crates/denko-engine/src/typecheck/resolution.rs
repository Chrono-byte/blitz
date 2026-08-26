// SPDX-License-Identifier: MPL-2.0

use super::*;

impl TypeContext {
  pub(super) fn resolve_type(
    &self,
    typ: &Type,
  ) -> Type {
    self.resolve_type_with_environment(typ, &TypeEnvironment::new(), 0)
  }

  pub(super) fn instantiate_type_parameters(
    &self,
    typ: &Type,
    environment: &TypeEnvironment,
  ) -> Type {
    match typ {
      Type::TypeReference { name, arguments } => {
        if let Some(bound) = environment.get(name) {
          return self.instantiate_type_parameters(bound, environment);
        }
        Type::TypeReference {
          name: name.clone(),
          arguments: arguments
            .iter()
            .map(|argument| self.instantiate_type_parameters(argument, environment))
            .collect(),
        }
      }
      Type::Array(element) => Type::Array(Box::new(
        self.instantiate_type_parameters(element, environment),
      )),
      Type::Tuple(elements) => Type::Tuple(
        elements
          .iter()
          .map(|element| self.instantiate_type_parameters(element, environment))
          .collect(),
      ),
      Type::Rest(element) => Type::Rest(Box::new(
        self.instantiate_type_parameters(element, environment),
      )),
      Type::Function(parameters, return_type) => Type::Function(
        parameters
          .iter()
          .map(|parameter| self.instantiate_type_parameters(parameter, environment))
          .collect(),
        Box::new(self.instantiate_type_parameters(return_type, environment)),
      ),
      Type::Object(properties) => Type::Object(
        properties
          .iter()
          .map(|(name, property)| {
            (
              name.clone(),
              self.instantiate_type_parameters(property, environment),
            )
          })
          .collect(),
      ),
      Type::Union(types) => Type::Union(
        types
          .iter()
          .map(|typ| self.instantiate_type_parameters(typ, environment))
          .collect(),
      ),
      Type::Intersection(types) => Type::Intersection(
        types
          .iter()
          .map(|typ| self.instantiate_type_parameters(typ, environment))
          .collect(),
      ),
      Type::Conditional {
        check,
        extends,
        true_type,
        false_type,
        distributive_parameter,
      } => Type::Conditional {
        check: Box::new(self.instantiate_type_parameters(check, environment)),
        extends: Box::new(self.instantiate_type_parameters(extends, environment)),
        true_type: Box::new(self.instantiate_type_parameters(true_type, environment)),
        false_type: Box::new(self.instantiate_type_parameters(false_type, environment)),
        distributive_parameter: distributive_parameter.clone(),
      },
      Type::KeyOf(value) => Type::KeyOf(Box::new(
        self.instantiate_type_parameters(value, environment),
      )),
      Type::IndexedAccess { object, index } => Type::IndexedAccess {
        object: Box::new(self.instantiate_type_parameters(object, environment)),
        index: Box::new(self.instantiate_type_parameters(index, environment)),
      },
      Type::Mapped {
        parameter,
        constraint,
        value,
      } => Type::Mapped {
        parameter: parameter.clone(),
        constraint: Box::new(self.instantiate_type_parameters(constraint, environment)),
        value: Box::new(self.instantiate_type_parameters(value, environment)),
      },
      Type::TemplateLiteral {
        quasis,
        substitutions,
      } => Type::TemplateLiteral {
        quasis: quasis.clone(),
        substitutions: substitutions
          .iter()
          .map(|substitution| self.instantiate_type_parameters(substitution, environment))
          .collect(),
      },
      _ => typ.clone(),
    }
  }

  pub(super) fn resolve_type_with_environment(
    &self,
    typ: &Type,
    environment: &TypeEnvironment,
    depth: usize,
  ) -> Type {
    if depth >= TYPE_EVALUATION_LIMIT {
      return Type::Unknown;
    }
    match typ {
      Type::TypeReference { name, arguments } => {
        if let Some(bound) = environment.get(name) {
          return self.resolve_type_with_environment(bound, environment, depth + 1);
        }
        if name == "Exclude" && arguments.len() >= 2 {
          let source = self.resolve_type_with_environment(&arguments[0], environment, depth + 1);
          let excluded = self.resolve_type_with_environment(&arguments[1], environment, depth + 1);
          return normalize_union(
            union_members(source)
              .into_iter()
              .filter(|member| !assignable(&excluded, member))
              .collect(),
          );
        }
        if name == "ReadonlyArray"
          && let Some(element) = arguments.first()
        {
          return Type::Array(Box::new(self.resolve_type_with_environment(
            element,
            environment,
            depth + 1,
          )));
        }
        if name == "Record" && arguments.len() >= 2 {
          let keys = self.resolve_type_with_environment(&arguments[0], environment, depth + 1);
          let value = self.resolve_type_with_environment(&arguments[1], environment, depth + 1);
          return Type::Object(
            union_members(keys)
              .into_iter()
              .filter_map(|key| property_key_from_type(&key).map(|key| (key, value.clone())))
              .collect(),
          );
        }
        let resolving_marker = format!("\0resolving:{name}");
        if environment.contains_key(&resolving_marker) {
          return Type::Unknown;
        }
        let definition = self
          .namespace_types
          .get(name)
          .cloned()
          .or_else(|| self.named_types.get(name).cloned());
        let Some(definition) = definition else {
          return Type::Unknown;
        };
        let mut definition_environment = environment.clone();
        definition_environment.insert(resolving_marker, Type::Unknown);
        for (index, parameter) in definition.parameters.iter().enumerate() {
          let argument = arguments
            .get(index)
            .cloned()
            .or_else(|| parameter.default.clone());
          let argument = argument.map_or(Type::Unknown, |typ| {
            self.resolve_type_with_environment(&typ, environment, depth + 1)
          });
          if let Some(constraint) = &parameter.constraint {
            let constraint =
              self.resolve_type_with_environment(constraint, &definition_environment, depth + 1);
            if !assignable(&constraint, &argument) {
              return Type::Never;
            }
          }
          definition_environment.insert(parameter.name.clone(), argument);
        }
        self.resolve_type_with_environment(&definition.body, &definition_environment, depth + 1)
      }
      Type::Array(element) => Type::Array(Box::new(self.resolve_type_with_environment(
        element,
        environment,
        depth + 1,
      ))),
      Type::Tuple(elements) => Type::Tuple(
        elements
          .iter()
          .map(|element| self.resolve_type_with_environment(element, environment, depth + 1))
          .collect(),
      ),
      Type::Rest(element) => Type::Rest(Box::new(self.resolve_type_with_environment(
        element,
        environment,
        depth + 1,
      ))),
      Type::Infer(_) => typ.clone(),
      Type::Object(properties) => Type::Object(
        properties
          .iter()
          .map(|(name, typ)| {
            (
              name.clone(),
              self.resolve_type_with_environment(typ, environment, depth + 1),
            )
          })
          .collect(),
      ),
      Type::Function(parameters, return_type) => Type::Function(
        parameters
          .iter()
          .map(|typ| self.resolve_type_with_environment(typ, environment, depth + 1))
          .collect(),
        Box::new(self.resolve_type_with_environment(return_type, environment, depth + 1)),
      ),
      Type::Union(types) => normalize_union(
        types
          .iter()
          .map(|typ| self.resolve_type_with_environment(typ, environment, depth + 1))
          .collect(),
      ),
      Type::Intersection(types) => self.resolve_intersection(types, environment, depth + 1),
      Type::Conditional {
        check,
        extends,
        true_type,
        false_type,
        distributive_parameter,
      } => {
        if let Some(parameter) = distributive_parameter
          && let Some(Type::Union(members)) = environment.get(parameter)
        {
          let distributed = members
            .iter()
            .map(|member| {
              let mut member_environment = environment.clone();
              member_environment.insert(parameter.clone(), member.clone());
              self.resolve_conditional(
                check,
                extends,
                true_type,
                false_type,
                &member_environment,
                depth + 1,
              )
            })
            .collect();
          return normalize_union(distributed);
        }
        self.resolve_conditional(
          check,
          extends,
          true_type,
          false_type,
          environment,
          depth + 1,
        )
      }
      Type::KeyOf(object) => {
        let object = self.resolve_type_with_environment(object, environment, depth + 1);
        match object {
          Type::Object(properties) => normalize_union(
            properties
              .keys()
              .map(|key| Type::Literal(LiteralType::String(key.clone())))
              .collect(),
          ),
          Type::Array(_) | Type::Tuple(_) => Type::Number,
          _ => Type::Unknown,
        }
      }
      Type::IndexedAccess { object, index } => {
        let object = self.resolve_type_with_environment(object, environment, depth + 1);
        let index = self.resolve_type_with_environment(index, environment, depth + 1);
        resolve_indexed_access(&object, &index)
      }
      Type::Mapped {
        parameter,
        constraint,
        value,
      } => {
        let constraint = self.resolve_type_with_environment(constraint, environment, depth + 1);
        let mut properties = HashMap::new();
        for key_type in union_members(constraint) {
          if let Some(key) = property_key_from_type(&key_type) {
            let mut mapped_environment = environment.clone();
            mapped_environment.insert(parameter.clone(), key_type);
            let value = self.resolve_type_with_environment(value, &mapped_environment, depth + 1);
            properties.insert(key, value);
          }
        }
        Type::Object(properties)
      }
      Type::TemplateLiteral {
        quasis,
        substitutions,
      } => {
        let substitutions: Vec<Type> = substitutions
          .iter()
          .map(|typ| self.resolve_type_with_environment(typ, environment, depth + 1))
          .collect();
        evaluate_template_literal(quasis, &substitutions)
      }
      _ => typ.clone(),
    }
  }

  pub(super) fn resolve_conditional(
    &self,
    check: &Type,
    extends: &Type,
    true_type: &Type,
    false_type: &Type,
    environment: &TypeEnvironment,
    depth: usize,
  ) -> Type {
    let check = self.resolve_type_with_environment(check, environment, depth + 1);
    let extends = self.resolve_type_with_environment(extends, environment, depth + 1);
    let mut inferred_environment = environment.clone();
    let (branch, branch_environment) =
      if self.match_type_pattern(&extends, &check, &mut inferred_environment, depth + 1) {
        (true_type, &inferred_environment)
      } else {
        (false_type, environment)
      };
    self.resolve_type_with_environment(branch, branch_environment, depth + 1)
  }

  pub(super) fn match_type_pattern(
    &self,
    pattern: &Type,
    actual: &Type,
    environment: &mut TypeEnvironment,
    depth: usize,
  ) -> bool {
    if depth >= TYPE_EVALUATION_LIMIT {
      return true;
    }
    match pattern {
      Type::Infer(name) => {
        environment.insert(name.clone(), actual.clone());
        true
      }
      Type::Tuple(pattern_elements) => {
        let Type::Tuple(actual_elements) = actual else {
          return false;
        };
        let rest_position = pattern_elements
          .iter()
          .position(|element| matches!(element, Type::Rest(_)));
        if rest_position.is_none() && pattern_elements.len() != actual_elements.len() {
          return false;
        }
        if let Some(position) = rest_position {
          if actual_elements.len() < position {
            return false;
          }
          for (pattern, actual) in pattern_elements[..position]
            .iter()
            .zip(actual_elements.iter())
          {
            if !self.match_type_pattern(pattern, actual, environment, depth + 1) {
              return false;
            }
          }
          let Type::Rest(rest) = &pattern_elements[position] else {
            return false;
          };
          if let Type::Infer(name) = rest.as_ref() {
            environment.insert(
              name.clone(),
              Type::Tuple(actual_elements[position..].to_vec()),
            );
            return true;
          }
          let rest_element = match rest.as_ref() {
            Type::Array(element) => element.as_ref(),
            other => other,
          };
          actual_elements[position..]
            .iter()
            .all(|actual| self.match_type_pattern(rest_element, actual, environment, depth + 1))
        } else {
          pattern_elements
            .iter()
            .zip(actual_elements.iter())
            .all(|(pattern, actual)| {
              self.match_type_pattern(pattern, actual, environment, depth + 1)
            })
        }
      }
      Type::Array(pattern_element) => match actual {
        Type::Array(actual_element) => {
          self.match_type_pattern(pattern_element, actual_element, environment, depth + 1)
        }
        Type::Tuple(actual_elements) => actual_elements
          .iter()
          .all(|actual| self.match_type_pattern(pattern_element, actual, environment, depth + 1)),
        _ => false,
      },
      _ => assignable(pattern, actual),
    }
  }

  pub(super) fn resolve_intersection(
    &self,
    types: &[Type],
    environment: &TypeEnvironment,
    depth: usize,
  ) -> Type {
    let resolved: Vec<Type> = types
      .iter()
      .map(|typ| self.resolve_type_with_environment(typ, environment, depth + 1))
      .collect();
    if resolved
      .iter()
      .all(|typ| matches!(typ, Type::Object(_)))
    {
      let mut properties = HashMap::new();
      for typ in resolved {
        if let Type::Object(object_properties) = typ {
          properties.extend(object_properties);
        }
      }
      Type::Object(properties)
    } else {
      Type::Intersection(resolved)
    }
  }

  pub(super) fn is_assignable(
    &self,
    expected: &Type,
    actual: &Type,
  ) -> bool {
    if let Some(assignable) = self.reference_assignability(expected, actual) {
      return assignable;
    }
    assignable(&self.resolve_type(expected), &self.resolve_type(actual))
  }

  pub(super) fn reference_assignability(
    &self,
    expected: &Type,
    actual: &Type,
  ) -> Option<bool> {
    let (
      Type::TypeReference {
        name: expected_name,
        arguments: expected_arguments,
      },
      Type::TypeReference {
        name: actual_name,
        arguments: actual_arguments,
      },
    ) = (expected, actual)
    else {
      return None;
    };
    let expected_definition = self
      .namespace_types
      .get(expected_name)
      .or_else(|| self.named_types.get(expected_name))?;
    let actual_definition = self
      .namespace_types
      .get(actual_name)
      .or_else(|| self.named_types.get(actual_name))?;
    if expected_definition != actual_definition
      || expected_definition
        .parameters
        .iter()
        .any(|parameter| parameter.variance == Variance::Unspecified)
    {
      return None;
    }
    let argument = |arguments: &[Type], index: usize, parameter: &TypeParameter| {
      arguments
        .get(index)
        .cloned()
        .or_else(|| parameter.default.clone())
        .unwrap_or(Type::Unknown)
    };
    Some(
      expected_definition
        .parameters
        .iter()
        .enumerate()
        .all(|(index, parameter)| {
          let expected = argument(expected_arguments, index, parameter);
          let actual = argument(actual_arguments, index, parameter);
          match parameter.variance {
            Variance::Out => self.is_assignable(&expected, &actual),
            Variance::In => self.is_assignable(&actual, &expected),
            Variance::InOut => {
              self.is_assignable(&expected, &actual) && self.is_assignable(&actual, &expected)
            }
            Variance::Unspecified => unreachable!("handled above"),
          }
        }),
    )
  }
}
