// SPDX-License-Identifier: MPL-2.0

use super::*;

impl TypeContext {
  pub(super) fn check_variable_declaration(
    &mut self,
    declaration: &oast::VariableDeclaration<'_>,
  ) -> Result<(), TypeCheckerError> {
    for declarator in &declaration.declarations {
      let name = match &declarator.id {
        oast::BindingPattern::BindingIdentifier(identifier) => identifier.name.to_string(),
        _ => {
          return Err(TypeCheckerError::from(
            "unsupported variable binding pattern",
          ));
        }
      };
      let declared_type = declarator
        .type_annotation
        .as_deref()
        .map(convert_type_ann)
        .transpose()?;
      let mut initializer_type = declarator
        .init
        .as_ref()
        .map(|initializer| self.type_of_expr_oast(initializer))
        .transpose()?;
      if declarator.type_annotation.is_none()
        && !matches!(declaration.kind, oast::VariableDeclarationKind::Const)
      {
        initializer_type = initializer_type.map(widen_literal);
      }
      let variable_type = declared_type
        .clone()
        .or_else(|| initializer_type.clone())
        .unwrap_or(Type::Any);

      if declarator.type_annotation.is_none()
        && declarator.init.is_some()
        && let Some(initializer_type) = initializer_type.as_ref()
        && let Some(initializer_type) = self.safe_inlay_type(initializer_type)
      {
        self.inlay_hints.push(TypeInlayHint {
          position: identifier_span_end(&declarator.id),
          label: format!(": {}", type_to_string(&initializer_type)),
          kind: TypeInlayHintKind::Type,
        });
      }

      if let (Some(expected), Some(actual)) = (&declared_type, &initializer_type)
        && !self.is_assignable(expected, actual)
      {
        return Err(TypeCheckerError::from(format!(
          "Type '{}' is not assignable to '{}' for variable '{}'",
          type_to_string(actual),
          type_to_string(expected),
          name
        )));
      }
      self.scope_mut().insert(name, variable_type);
    }
    Ok(())
  }

  pub(super) fn check_function(
    &mut self,
    function: &oast::Function<'_>,
  ) -> Result<Type, TypeCheckerError> {
    let Some(body) = &function.body else {
      return self.function_type(&function.params, function.return_type.as_deref());
    };

    self.check_function_body(
      &function.params,
      body,
      function.return_type.as_deref(),
      function.r#async || function.generator,
      function.params.span.end as usize,
      None,
    )
  }

  pub(super) fn check_arrow_function(
    &mut self,
    function: &oast::ArrowFunctionExpression<'_>,
  ) -> Result<Type, TypeCheckerError> {
    let hint_checkpoint = self.inlay_hints.len();
    let implicit_return = if function.expression {
      match function.body.statements.as_slice() {
        [oast::Statement::ExpressionStatement(statement)] => Some(&statement.expression),
        _ => None,
      }
    } else {
      None
    };
    match self.check_function_body(
      &function.params,
      &function.body,
      function.return_type.as_deref(),
      function.r#async,
      function.params.span.end as usize,
      implicit_return,
    ) {
      Ok(typ) => Ok(typ),
      Err(_) => {
        self.inlay_hints.truncate(hint_checkpoint);
        self.function_type(&function.params, function.return_type.as_deref())
      }
    }
  }

  pub(super) fn check_function_body(
    &mut self,
    params: &oast::FormalParameters<'_>,
    body: &oast::FunctionBody<'_>,
    explicit_return_type: Option<&oast::TSTypeAnnotation<'_>>,
    async_or_generator: bool,
    return_hint_position: usize,
    implicit_return: Option<&oast::Expression<'_>>,
  ) -> Result<Type, TypeCheckerError> {
    let explicit_return_type = explicit_return_type
      .map(convert_type_ann)
      .transpose()?;

    if strict_mode() {
      for parameter in &params.items {
        if parameter.type_annotation.is_none()
          && let oast::BindingPattern::BindingIdentifier(identifier) = &parameter.pattern
        {
          return Err(TypeCheckerError::from(format!(
            "Parameter '{}' implicitly has type 'any'",
            identifier.name
          )));
        }
      }
    }

    self.scopes.push(Scope::new());
    for parameter in &params.items {
      if let oast::BindingPattern::BindingIdentifier(identifier) = &parameter.pattern {
        let typ = parameter
          .type_annotation
          .as_deref()
          .map(convert_type_ann)
          .transpose()?
          .unwrap_or(Type::Any);
        self
          .scope_mut()
          .insert(identifier.name.to_string(), typ);
      }
    }
    if let Some(rest) = &params.rest
      && let oast::BindingPattern::BindingIdentifier(identifier) = &rest.rest.argument
    {
      let typ = rest
        .type_annotation
        .as_deref()
        .map(convert_type_ann)
        .transpose()?
        .unwrap_or(Type::Any);
      self
        .scope_mut()
        .insert(identifier.name.to_string(), typ);
    }
    self
      .expected_returns
      .push(explicit_return_type.clone().unwrap_or(Type::Any));
    self.return_types.push(Vec::new());
    let result = if let Some(expression) = implicit_return {
      self.type_of_expr_oast(expression).map(|typ| {
        self
          .return_types
          .last_mut()
          .expect("return type stack is empty")
          .push(typ);
      })
    } else {
      body
        .statements
        .iter()
        .try_for_each(|statement| self.check_statement_oast(statement))
    };
    let inferred_returns = self
      .return_types
      .pop()
      .expect("return type stack is empty");
    self.expected_returns.pop();
    self.scopes.pop();

    result?;
    let inferred_return_type = if explicit_return_type.is_none() && !async_or_generator {
      self.infer_simple_return_type(body, implicit_return, &inferred_returns)
    } else {
      None
    };
    if let Some(return_type) = inferred_return_type.as_ref() {
      self.inlay_hints.push(TypeInlayHint {
        position: return_hint_position,
        label: format!(": {}", type_to_string(return_type)),
        kind: TypeInlayHintKind::Type,
      });
    }
    let typ = Type::Function(
      self.parameter_types(params)?,
      Box::new(
        explicit_return_type
          .or(inferred_return_type)
          .unwrap_or(Type::Any),
      ),
    );
    self.record_function_parameter_metadata(&typ, params);
    Ok(typ)
  }

  pub(super) fn parameter_types(
    &self,
    params: &oast::FormalParameters<'_>,
  ) -> Result<Vec<Type>, TypeCheckerError> {
    let mut parameter_types = Vec::new();
    for parameter in &params.items {
      parameter_types.push(
        parameter
          .type_annotation
          .as_deref()
          .map(convert_type_ann)
          .transpose()?
          .unwrap_or(Type::Any),
      );
    }
    if let Some(rest) = &params.rest {
      parameter_types.push(
        rest
          .type_annotation
          .as_deref()
          .map(convert_type_ann)
          .transpose()?
          .unwrap_or(Type::Any),
      );
    }
    Ok(parameter_types)
  }

  pub(super) fn infer_simple_return_type(
    &self,
    body: &oast::FunctionBody<'_>,
    implicit_return: Option<&oast::Expression<'_>>,
    returns: &[Type],
  ) -> Option<Type> {
    let simple_body = implicit_return.is_some()
      || body.statements.is_empty()
      || (body
        .statements
        .last()
        .is_some_and(|statement| matches!(statement, oast::Statement::ReturnStatement(_)))
        && body.statements[..body.statements.len().saturating_sub(1)]
          .iter()
          .all(|statement| matches!(statement, oast::Statement::VariableDeclaration(_))));
    if !simple_body {
      return None;
    }
    let inferred = returns.first().cloned().unwrap_or(Type::Void);
    if returns.len() > 1 {
      return None;
    }
    self.safe_inlay_type(&widen_literal(inferred))
  }

  pub(super) fn safe_inlay_type(
    &self,
    typ: &Type,
  ) -> Option<Type> {
    let typ = self.resolve_type(typ);
    if !is_supported_inlay_type(&typ) {
      return None;
    }
    Some(typ)
  }

  pub(super) fn check_exported_declaration(
    &mut self,
    declaration: &oast::Declaration<'_>,
  ) -> Result<(), TypeCheckerError> {
    match declaration {
      oast::Declaration::VariableDeclaration(variable) => self.check_variable_declaration(variable),
      oast::Declaration::FunctionDeclaration(function) => self.check_function(function).map(|_| ()),
      _ => Ok(()),
    }
  }

  /// Checks a single OAST statement and updates the type context accordingly.
  ///
  /// # Errors
  ///
  /// Returns a `TypeCheckerError` if the statement cannot be typechecked.
  #[allow(clippy::too_many_lines)]
  pub(super) fn check_statement_oast(
    &mut self,
    stmt: &oast::Statement,
  ) -> Result<(), TypeCheckerError> {
    use oast::Statement as OStmt;
    match stmt {
      OStmt::VariableDeclaration(variable) => self.check_variable_declaration(variable)?,
      OStmt::FunctionDeclaration(function) => {
        self.check_function(function)?;
      }
      OStmt::ExportNamedDeclaration(export) => {
        if let Some(declaration) = &export.declaration {
          self.check_exported_declaration(declaration)?;
        }
      }
      OStmt::ExpressionStatement(e) => {
        drop(self.type_of_expr_oast(&e.expression)?);
      }
      OStmt::BlockStatement(b) => {
        self.scopes.push(Scope::new());
        let result = b
          .body
          .iter()
          .try_for_each(|statement| self.check_statement_oast(statement));
        self.scopes.pop();
        result?;
      }
      OStmt::IfStatement(i) => {
        drop(self.type_of_expr_oast(&i.test)?);
        self.check_statement_oast(&i.consequent)?;
        if let Some(alt) = &i.alternate {
          self.check_statement_oast(alt)?;
        }
      }
      OStmt::WhileStatement(w) => {
        drop(self.type_of_expr_oast(&w.test)?);
        self.check_statement_oast(&w.body)?;
      }
      OStmt::ReturnStatement(r) => {
        let actual = r
          .argument
          .as_ref()
          .map(|argument| self.type_of_expr_oast(argument))
          .transpose()?
          .unwrap_or(Type::Void);
        if let Some(expected) = self.expected_returns.last()
          && !self.is_assignable(expected, &actual)
        {
          return Err(TypeCheckerError::from(format!(
            "Type '{}' is not assignable to function return type '{}'",
            type_to_string(&actual),
            type_to_string(expected)
          )));
        }
        if let Some(returns) = self.return_types.last_mut() {
          returns.push(actual);
        }
      }
      _ => {}
    }
    Ok(())
  }

  pub(super) fn type_of_expr_with_context(
    &mut self,
    expression: &oast::Expression<'_>,
    expected: &Type,
  ) -> Result<Type, TypeCheckerError> {
    let expected = self.resolve_type(expected);
    match (expression, &expected) {
      (
        oast::Expression::ArrowFunctionExpression(function),
        Type::Function(parameters, return_type),
      ) => {
        self.scopes.push(Scope::new());
        for (index, parameter) in function.params.items.iter().enumerate() {
          let typ = parameters
            .get(index)
            .cloned()
            .unwrap_or(Type::Any);
          if let oast::BindingPattern::BindingIdentifier(identifier) = &parameter.pattern {
            if parameter.type_annotation.is_none()
              && !matches!(typ, Type::Any | Type::Unknown | Type::Never)
            {
              self.inlay_hints.push(TypeInlayHint {
                position: identifier.span.end as usize,
                label: format!(": {}", type_to_string(&typ)),
                kind: TypeInlayHintKind::Type,
              });
            }
            self
              .scope_mut()
              .insert(identifier.name.to_string(), typ);
          }
        }
        self.scopes.pop();
        Ok(Type::Function(parameters.clone(), return_type.clone()))
      }
      (oast::Expression::ObjectExpression(object), Type::Object(expected_properties)) => {
        let mut properties = HashMap::new();
        for property in &object.properties {
          let oast::ObjectPropertyKind::ObjectProperty(property) = property else {
            continue;
          };
          let name = match &property.key {
            oast::PropertyKey::StaticIdentifier(identifier) => Some(identifier.name.as_str()),
            oast::PropertyKey::StringLiteral(literal) => Some(literal.value.as_str()),
            _ => None,
          };
          let typ =
            if let Some(expected_property) = name.and_then(|name| expected_properties.get(name)) {
              self.type_of_expr_with_context(&property.value, expected_property)?
            } else {
              self.type_of_expr_oast(&property.value)?
            };
          if let Some(name) = name {
            properties.insert(name.to_string(), typ);
          }
        }
        Ok(Type::Object(properties))
      }
      _ => self.type_of_expr_oast(expression),
    }
  }

  pub(super) fn generic_pattern(
    &self,
    typ: &Type,
    environment: &TypeEnvironment,
    depth: usize,
  ) -> Type {
    if depth >= TYPE_EVALUATION_LIMIT {
      return typ.clone();
    }
    match typ {
      Type::TypeReference { name, arguments } => {
        if let Some(bound) = environment.get(name) {
          return self.generic_pattern(bound, environment, depth + 1);
        }
        let definition = self
          .namespace_types
          .get(name)
          .or_else(|| self.named_types.get(name));
        if let Some(definition) = definition {
          if !definition.parameters.is_empty()
            && definition
              .parameters
              .iter()
              .all(|parameter| parameter.variance != Variance::Unspecified)
          {
            return Type::TypeReference {
              name: name.clone(),
              arguments: arguments
                .iter()
                .map(|argument| self.generic_pattern(argument, environment, depth + 1))
                .collect(),
            };
          }
          let mut definition_environment = environment.clone();
          for (index, parameter) in definition.parameters.iter().enumerate() {
            if let Some(argument) = arguments
              .get(index)
              .or(parameter.default.as_ref())
            {
              definition_environment.insert(
                parameter.name.clone(),
                self.generic_pattern(argument, environment, depth + 1),
              );
            }
          }
          return self.generic_pattern(&definition.body, &definition_environment, depth + 1);
        }
        Type::TypeReference {
          name: name.clone(),
          arguments: arguments
            .iter()
            .map(|argument| self.generic_pattern(argument, environment, depth + 1))
            .collect(),
        }
      }
      Type::Array(element) => Type::Array(Box::new(self.generic_pattern(
        element,
        environment,
        depth + 1,
      ))),
      Type::Tuple(elements) => Type::Tuple(
        elements
          .iter()
          .map(|element| self.generic_pattern(element, environment, depth + 1))
          .collect(),
      ),
      Type::Rest(element) => Type::Rest(Box::new(self.generic_pattern(
        element,
        environment,
        depth + 1,
      ))),
      Type::Function(parameters, return_type) => Type::Function(
        parameters
          .iter()
          .map(|parameter| self.generic_pattern(parameter, environment, depth + 1))
          .collect(),
        Box::new(self.generic_pattern(return_type, environment, depth + 1)),
      ),
      Type::Object(properties) => Type::Object(
        properties
          .iter()
          .map(|(name, property)| {
            (
              name.clone(),
              self.generic_pattern(property, environment, depth + 1),
            )
          })
          .collect(),
      ),
      Type::Union(types) => Type::Union(
        types
          .iter()
          .map(|typ| self.generic_pattern(typ, environment, depth + 1))
          .collect(),
      ),
      Type::Intersection(types) => Type::Intersection(
        types
          .iter()
          .map(|typ| self.generic_pattern(typ, environment, depth + 1))
          .collect(),
      ),
      _ => typ.clone(),
    }
  }

  pub(super) fn call_type(
    &mut self,
    callee: &Type,
    call: &oast::CallExpression<'_>,
  ) -> Result<Type, TypeCheckerError> {
    let resolved_callee;
    let callable = if matches!(callee, Type::TypeReference { .. }) {
      resolved_callee = self.resolve_type(callee);
      &resolved_callee
    } else {
      callee
    };
    let mut candidates = Vec::new();
    flatten_union_types(callable, &mut candidates);
    let explicit_types = call
      .type_arguments
      .as_ref()
      .map(|arguments| {
        arguments
          .params
          .iter()
          .map(convert_ts_type)
          .collect::<Result<Vec<_>, _>>()
      })
      .transpose()?;
    let mut best: Option<(usize, String, Type, Vec<TypeInlayHint>)> = None;
    for candidate in candidates {
      let metadata = self
        .function_parameter_metadata(&candidate)
        .cloned();
      let Type::Function(parameters, return_type) = candidate else {
        continue;
      };
      let rest_index = metadata
        .as_ref()
        .and_then(|metadata| metadata.rest_index);
      if !parameters.is_empty()
        && match rest_index {
          Some(rest_index) => call.arguments.len() < rest_index,
          None => parameters.len() != call.arguments.len(),
        }
      {
        continue;
      }
      let hint_checkpoint = self.inlay_hints.len();
      let candidate_result: Result<_, TypeCheckerError> = (|| {
        let mut environment = TypeEnvironment::new();
        let mut generic_names = Vec::new();
        for parameter in &parameters {
          collect_generic_names(parameter, &self.named_types, &mut generic_names);
        }
        collect_generic_names(&return_type, &self.named_types, &mut generic_names);
        if let Some(explicit_types) = &explicit_types {
          for (name, typ) in generic_names.iter().zip(explicit_types) {
            environment.insert(name.clone(), typ.clone());
          }
        }

        // Infer from the uncontextualized arguments first. This lets a sibling
        // property such as `value: T` instantiate `T` before a callback is
        // contextually typed as `(value: T) => ...`.
        if !generic_names.is_empty() {
          let mut raw_arguments = Vec::new();
          for argument in &call.arguments {
            let expression = argument
              .as_expression()
              .ok_or_else(|| TypeCheckerError::from("Unsupported call argument"))?;
            raw_arguments.push(self.type_of_expr_oast(expression)?);
          }
          self.inlay_hints.truncate(hint_checkpoint);
          for (index, argument) in raw_arguments.iter().enumerate() {
            let Some(parameter) = call_parameter(&parameters, rest_index, index) else {
              continue;
            };
            let pattern = self.generic_pattern(parameter, &environment, 0);
            let actual = self.generic_pattern(argument, &TypeEnvironment::new(), 0);
            infer_type_parameter(&pattern, &actual, &mut environment);
          }
        }
        for name in &generic_names {
          environment
            .entry(name.clone())
            .or_insert(Type::Never);
        }

        let mut arguments = Vec::new();
        for (index, argument) in call.arguments.iter().enumerate() {
          let expression = argument
            .as_expression()
            .ok_or_else(|| TypeCheckerError::from("Unsupported call argument"))?;
          arguments.push(
            if let Some(parameter) = call_parameter(&parameters, rest_index, index) {
              let parameter = self.instantiate_type_parameters(parameter, &environment);
              self.type_of_expr_with_context(expression, rest_argument_type(&parameter))?
            } else {
              self.type_of_expr_oast(expression)?
            },
          );
        }
        for (index, argument) in arguments.iter().enumerate() {
          let Some(parameter) = call_parameter(&parameters, rest_index, index) else {
            continue;
          };
          let pattern = self.generic_pattern(parameter, &environment, 0);
          let actual = self.generic_pattern(argument, &TypeEnvironment::new(), 0);
          infer_type_parameter(&pattern, &actual, &mut environment);
        }
        let compatible = arguments
          .iter()
          .enumerate()
          .all(|(index, argument)| {
            let Some(parameter) = call_parameter(&parameters, rest_index, index) else {
              return false;
            };
            let parameter = self.instantiate_type_parameters(parameter, &environment);
            let actual = argument.clone();
            self.is_assignable(rest_argument_type(&parameter), &actual)
          });
        if !compatible {
          return Ok(None);
        }
        if let Some(metadata) = &metadata {
          for (index, argument) in call.arguments.iter().enumerate() {
            let Some(name) = metadata
              .names
              .get(index)
              .and_then(Option::as_deref)
            else {
              continue;
            };
            if metadata
              .rest_index
              .is_some_and(|rest_index| index >= rest_index)
              || suppress_parameter_hint(argument, name)
            {
              continue;
            }
            self.inlay_hints.push(TypeInlayHint {
              position: argument.span().start as usize,
              label: format!("{name}:"),
              kind: TypeInlayHintKind::Parameter,
            });
          }
        }
        let return_type = self.instantiate_type_parameters(&return_type, &environment);
        let score = parameters.iter().map(type_specificity).sum();
        let key = parameters
          .iter()
          .map(type_to_string)
          .collect::<Vec<_>>()
          .join(" | ");
        Ok(Some((return_type, score, key)))
      })();
      let hints = if candidate_result.is_ok() {
        self.inlay_hints[hint_checkpoint..].to_vec()
      } else {
        Vec::new()
      };
      self.inlay_hints.truncate(hint_checkpoint);
      let Ok(Some((return_type, score, key))) = candidate_result else {
        continue;
      };
      let replace = match best.as_ref() {
        None => true,
        Some((best_score, best_key, _, _)) => {
          score > *best_score || (score == *best_score && key < *best_key)
        }
      };
      if replace {
        best = Some((score, key, return_type, hints));
      }
    }
    if let Some((_, _, return_type, hints)) = best {
      self.inlay_hints.extend(hints);
      return Ok(return_type);
    }
    Err(TypeCheckerError::from(format!(
      "No overload matches call for {}",
      type_to_string(callee)
    )))
  }

  /// Returns the type of the given OAST expression.
  ///
  /// # Errors
  ///
  /// Returns a `TypeCheckerError` if the expression cannot be typechecked.
  #[allow(clippy::too_many_lines)]
  pub(super) fn type_of_expr_oast(
    &mut self,
    expr: &oast::Expression,
  ) -> Result<Type, TypeCheckerError> {
    use oast::Expression as OExpr;
    match expr {
      OExpr::NumericLiteral(literal) => {
        Ok(Type::Literal(LiteralType::Number(literal.value.to_bits())))
      }
      OExpr::StringLiteral(literal) => Ok(Type::Literal(LiteralType::String(
        literal.value.to_string(),
      ))),
      OExpr::BooleanLiteral(literal) => Ok(Type::Literal(LiteralType::Boolean(literal.value))),
      OExpr::NullLiteral(_) => Ok(Type::Null),
      OExpr::Identifier(id) => {
        let name = id.name.as_str();
        self
          .lookup(name)
          .ok_or_else(|| TypeCheckerError::from(format!("Unknown identifier '{name}'")))
      }
      OExpr::UnaryExpression(u) => {
        let a = widen_literal(self.type_of_expr_oast(&u.argument)?);
        match u.operator {
          oast::UnaryOperator::LogicalNot => Ok(Type::Boolean),
          oast::UnaryOperator::Typeof => Ok(Type::String),
          oast::UnaryOperator::Void => Ok(Type::Void),
          oast::UnaryOperator::Delete => Ok(Type::Boolean),
          oast::UnaryOperator::UnaryPlus
          | oast::UnaryOperator::UnaryNegation
          | oast::UnaryOperator::BitwiseNot => {
            if self.is_assignable(&Type::Number, &a) {
              Ok(Type::Number)
            } else {
              Err(TypeCheckerError::from(
                "numeric unary operator requires number",
              ))
            }
          }
        }
      }
      OExpr::BinaryExpression(b) => {
        let l = widen_literal(self.type_of_expr_oast(&b.left)?);
        let r = widen_literal(self.type_of_expr_oast(&b.right)?);
        match b.operator {
          oast::BinaryOperator::Addition => {
            if matches!(l, Type::Any) || matches!(r, Type::Any) {
              Ok(Type::Any)
            } else if l == Type::Number && r == Type::Number {
              Ok(Type::Number)
            } else if l == Type::String || r == Type::String {
              Ok(Type::String)
            } else {
              Err(TypeCheckerError::from(format!(
                "Cannot add {} and {}",
                type_to_string(&l),
                type_to_string(&r)
              )))
            }
          }
          oast::BinaryOperator::Subtraction
          | oast::BinaryOperator::Multiplication
          | oast::BinaryOperator::Division
          | oast::BinaryOperator::Remainder
          | oast::BinaryOperator::Exponential
          | oast::BinaryOperator::ShiftLeft
          | oast::BinaryOperator::ShiftRight
          | oast::BinaryOperator::ShiftRightZeroFill
          | oast::BinaryOperator::BitwiseOR
          | oast::BinaryOperator::BitwiseXOR
          | oast::BinaryOperator::BitwiseAnd => {
            if self.is_assignable(&Type::Number, &l) && self.is_assignable(&Type::Number, &r) {
              Ok(Type::Number)
            } else {
              Err(TypeCheckerError::from(format!(
                "Arithmetic requires number, got {} and {}",
                type_to_string(&l),
                type_to_string(&r)
              )))
            }
          }
          oast::BinaryOperator::Equality
          | oast::BinaryOperator::Inequality
          | oast::BinaryOperator::StrictEquality
          | oast::BinaryOperator::StrictInequality
          | oast::BinaryOperator::LessThan
          | oast::BinaryOperator::LessEqualThan
          | oast::BinaryOperator::GreaterThan
          | oast::BinaryOperator::GreaterEqualThan
          | oast::BinaryOperator::In
          | oast::BinaryOperator::Instanceof => Ok(Type::Boolean),
        }
      }
      OExpr::AssignmentExpression(assignment) => {
        let actual = self.type_of_expr_oast(&assignment.right)?;
        if assignment.operator == oast::AssignmentOperator::Assign
          && let oast::AssignmentTarget::AssignmentTargetIdentifier(identifier) = &assignment.left
        {
          let name = identifier.name.as_str();
          if let Some(expected) = self.lookup(name)
            && !self.is_assignable(&expected, &actual)
          {
            return Err(TypeCheckerError::from(format!(
              "Type '{}' is not assignable to '{}' for variable '{}'",
              type_to_string(&actual),
              type_to_string(&expected),
              name
            )));
          }
        }
        Ok(actual)
      }
      OExpr::LogicalExpression(l) => {
        let left = self.type_of_expr_oast(&l.left)?;
        let right = self.type_of_expr_oast(&l.right)?;
        if self.is_assignable(&left, &right) {
          Ok(left)
        } else if self.is_assignable(&right, &left) {
          Ok(right)
        } else {
          Ok(Type::Union(vec![left, right]))
        }
      }
      OExpr::NewExpression(expression) => {
        let callee_type = self.type_of_expr_oast(&expression.callee)?;
        for argument in &expression.arguments {
          if let Some(argument) = argument.as_expression() {
            drop(self.type_of_expr_oast(argument)?);
          }
        }
        match callee_type {
          Type::Function(..) | Type::Any => Ok(Type::Any),
          Type::Unknown => Err(TypeCheckerError::from("Cannot construct unknown")),
          other => Err(TypeCheckerError::from(format!(
            "Cannot construct non-constructor type {}",
            type_to_string(&other)
          ))),
        }
      }
      OExpr::CallExpression(c) => {
        let callee_type = self.type_of_expr_oast(&c.callee)?;
        let is_console_log = if let OExpr::StaticMemberExpression(m) = &c.callee {
          if let OExpr::Identifier(obj_ident) = &m.object {
            let obj = obj_ident.name.as_str();
            obj == "console" && m.property.name.as_str() == "log"
          } else {
            false
          }
        } else {
          false
        };
        if is_console_log {
          return Ok(Type::Void);
        }
        match &callee_type {
          Type::Function(..) | Type::TypeReference { .. } | Type::Union(_) => {
            self.call_type(&callee_type, c)
          }
          Type::Any => Ok(Type::Any),
          Type::Unknown => Err(TypeCheckerError::from("Cannot call unknown")),
          _ => Err(TypeCheckerError::from(format!(
            "Cannot call non-function type {}",
            type_to_string(&callee_type)
          ))),
        }
      }
      OExpr::StaticMemberExpression(m) => {
        let inferred_object_type = self.type_of_expr_oast(&m.object)?;
        let obj_type = if matches!(inferred_object_type, Type::Object(_)) {
          inferred_object_type
        } else {
          self.resolve_type(&inferred_object_type)
        };
        match obj_type {
          Type::Object(ref props) => {
            let key = m.property.name.to_string();
            if let Some(t) = props.get(&key) {
              Ok(t.clone())
            } else {
              Err(TypeCheckerError::from(format!(
                "Property '{}' does not exist on type object",
                key
              )))
            }
          }
          Type::Any => Ok(Type::Any),
          Type::Unknown => Err(TypeCheckerError::from(
            "Cannot read a property from unknown",
          )),
          _ => Err(TypeCheckerError::from(format!(
            "Property '{}' does not exist on type {}",
            m.property.name,
            type_to_string(&obj_type)
          ))),
        }
      }
      OExpr::ComputedMemberExpression(member) => {
        let inferred_object_type = self.type_of_expr_oast(&member.object)?;
        match self.resolve_type(&inferred_object_type) {
          Type::Array(element) => Ok(*element),
          Type::Any => Ok(Type::Any),
          _ => Err(TypeCheckerError::from(
            "Computed member requires an array or indexable object",
          )),
        }
      }
      OExpr::ArrayExpression(arr) => {
        if arr.elements.is_empty() {
          return Ok(Type::Array(Box::new(Type::Any)));
        }
        let first_expr = arr.elements[0]
          .as_expression()
          .ok_or_else(|| TypeCheckerError::from("Unsupported array element"))?;
        let first = widen_literal(self.type_of_expr_oast(first_expr)?);
        for e in &arr.elements[1..] {
          let expr = e
            .as_expression()
            .ok_or_else(|| TypeCheckerError::from("Unsupported array element"))?;
          let t = widen_literal(self.type_of_expr_oast(expr)?);
          if !self.is_assignable(&first, &t) && !self.is_assignable(&t, &first) {
            return Ok(Type::Array(Box::new(Type::Union(vec![first, t]))));
          }
        }
        Ok(Type::Array(Box::new(first)))
      }
      OExpr::ThisExpression(_) => Ok(Type::Object(HashMap::new())),
      OExpr::ObjectExpression(obj) => {
        let mut props = HashMap::new();
        for p in &obj.properties {
          if let oast::ObjectPropertyKind::ObjectProperty(prop) = p {
            if prop.computed {
              continue;
            }
            let t = self.type_of_expr_oast(&prop.value)?;
            match &prop.key {
              oast::PropertyKey::StaticIdentifier(ident) => {
                props.insert(ident.name.to_string(), t);
              }
              oast::PropertyKey::StringLiteral(s) => {
                props.insert(s.value.to_string(), t);
              }
              _ => {}
            }
          }
        }
        Ok(Type::Object(props))
      }
      OExpr::ParenthesizedExpression(p) => self.type_of_expr_oast(&p.expression),
      OExpr::TemplateLiteral(_) => Ok(Type::String),
      OExpr::ConditionalExpression(expression) => {
        drop(self.type_of_expr_oast(&expression.test)?);
        let consequent = self.type_of_expr_oast(&expression.consequent)?;
        let alternate = self.type_of_expr_oast(&expression.alternate)?;
        if self.is_assignable(&consequent, &alternate) {
          Ok(consequent)
        } else if self.is_assignable(&alternate, &consequent) {
          Ok(alternate)
        } else {
          Ok(Type::Union(vec![consequent, alternate]))
        }
      }
      OExpr::TSAsExpression(expression) => convert_ts_type(&expression.type_annotation),
      OExpr::TSTypeAssertion(expression) => convert_ts_type(&expression.type_annotation),
      OExpr::TSNonNullExpression(expression) => self.type_of_expr_oast(&expression.expression),
      OExpr::FunctionExpression(function) => self.check_function(function),
      OExpr::ArrowFunctionExpression(function) => self.check_arrow_function(function),
      _ => Err(TypeCheckerError::from(
        "Unsupported expression in type checker",
      )),
    }
  }
}
