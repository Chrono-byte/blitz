// SPDX-License-Identifier: MPL-2.0

use super::*;

impl Compiler {
  pub(super) fn lexical_declared_names_of_statement(
    stmt: &oast::Statement<'_>,
    out: &mut HashSet<String>,
    function_body: bool,
  ) {
    match stmt {
      oast::Statement::VariableDeclaration(v)
        if !matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var) =>
      {
        for decl in &v.declarations {
          Self::collect_binding_names(&decl.id, out);
        }
      }
      oast::Statement::FunctionDeclaration(f) if !function_body => {
        if let Some(id) = &f.id {
          out.insert(id.name.to_string());
        }
      }
      oast::Statement::ClassDeclaration(c) => {
        if let Some(id) = &c.id {
          out.insert(id.name.to_string());
        }
      }
      _ => {}
    }
  }

  pub(super) fn collect_binding_names(
    pattern: &oast::BindingPattern<'_>,
    out: &mut HashSet<String>,
  ) {
    match pattern {
      oast::BindingPattern::BindingIdentifier(i) => {
        out.insert(i.name.to_string());
      }
      oast::BindingPattern::AssignmentPattern(a) => Self::collect_binding_names(&a.left, out),
      oast::BindingPattern::ObjectPattern(o) => {
        for prop in &o.properties {
          Self::collect_binding_names(&prop.value, out);
        }
        if let Some(rest) = &o.rest {
          Self::collect_binding_names(&rest.argument, out);
        }
      }
      oast::BindingPattern::ArrayPattern(a) => {
        for elem in a.elements.iter().flatten() {
          Self::collect_binding_names(elem, out);
        }
        if let Some(rest) = &a.rest {
          Self::collect_binding_names(&rest.argument, out);
        }
      }
    }
  }

  pub(super) fn var_declared_names_of_statement(
    &self,
    stmt: &oast::Statement<'_>,
    out: &mut HashSet<String>,
  ) {
    match stmt {
      oast::Statement::VariableDeclaration(v)
        if matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var) =>
      {
        for decl in &v.declarations {
          Self::collect_binding_names(&decl.id, out);
        }
      }
      oast::Statement::BlockStatement(b) => {
        for stmt in &b.body {
          self.var_declared_names_of_statement(stmt, out);
        }
      }
      oast::Statement::IfStatement(i) => {
        self.var_declared_names_of_statement(&i.consequent, out);
        if let Some(alt) = &i.alternate {
          self.var_declared_names_of_statement(alt, out);
        }
      }
      oast::Statement::WhileStatement(w) => self.var_declared_names_of_statement(&w.body, out),
      oast::Statement::DoWhileStatement(d) => self.var_declared_names_of_statement(&d.body, out),
      oast::Statement::ForStatement(f) => {
        if let Some(init) = &f.init
          && let oxc_ast::ast::ForStatementInit::VariableDeclaration(v) = init
          && matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var)
        {
          for decl in &v.declarations {
            Self::collect_binding_names(&decl.id, out);
          }
        }
        self.var_declared_names_of_statement(&f.body, out);
      }
      oast::Statement::ForInStatement(f) => {
        if let oxc_ast::ast::ForStatementLeft::VariableDeclaration(v) = &f.left
          && matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var)
        {
          for decl in &v.declarations {
            Self::collect_binding_names(&decl.id, out);
          }
        }
        self.var_declared_names_of_statement(&f.body, out);
      }
      oast::Statement::ForOfStatement(f) => {
        if let oxc_ast::ast::ForStatementLeft::VariableDeclaration(v) = &f.left
          && matches!(v.kind, oxc_ast::ast::VariableDeclarationKind::Var)
        {
          for decl in &v.declarations {
            Self::collect_binding_names(&decl.id, out);
          }
        }
        self.var_declared_names_of_statement(&f.body, out);
      }
      oast::Statement::LabeledStatement(l) => self.var_declared_names_of_statement(&l.body, out),
      oast::Statement::WithStatement(w) => self.var_declared_names_of_statement(&w.body, out),
      oast::Statement::TryStatement(t) => {
        for stmt in &t.block.body {
          self.var_declared_names_of_statement(stmt, out);
        }
        if let Some(handler) = &t.handler {
          for stmt in &handler.body.body {
            self.var_declared_names_of_statement(stmt, out);
          }
        }
        if let Some(finalizer) = &t.finalizer {
          for stmt in &finalizer.body {
            self.var_declared_names_of_statement(stmt, out);
          }
        }
      }
      _ => {}
    }
  }

  pub(super) fn validate_statement_position_function(
    &self,
    stmt: &oast::Statement<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    if matches!(stmt, oast::Statement::FunctionDeclaration(_)) {
      return Err(crate::errors::CompilerError::from(
        "SyntaxError: function declaration in statement position",
      ));
    }
    Ok(())
  }

  pub(super) fn validate_variable_initializers(
    &self,
    declaration: &oast::VariableDeclaration<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    for declarator in &declaration.declarations {
      if let Some(init) = &declarator.init {
        self.validate_early_errors_in_expression(init)?;
      }
    }
    Ok(())
  }

  pub(super) fn validate_assignment_target(
    &self,
    target: &oast::AssignmentTarget<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    match target {
      oast::AssignmentTarget::AssignmentTargetIdentifier(identifier) => {
        if let Some(error) =
          Self::strict_binding_name_error(self.strict_context, identifier.name.as_str())
        {
          return Err(error);
        }
        Ok(())
      }
      oast::AssignmentTarget::StaticMemberExpression(member) => {
        self.validate_early_errors_in_expression(&member.object)
      }
      oast::AssignmentTarget::ComputedMemberExpression(member) => {
        self.validate_early_errors_in_expression(&member.object)?;
        self.validate_early_errors_in_expression(&member.expression)
      }
      oast::AssignmentTarget::ArrayAssignmentTarget(array) => {
        for element in array.elements.iter().flatten() {
          self.validate_assignment_target_maybe_default(element)?;
        }
        if let Some(rest) = &array.rest {
          self.validate_assignment_target(&rest.target)?;
        }
        Ok(())
      }
      oast::AssignmentTarget::ObjectAssignmentTarget(object) => {
        for property in &object.properties {
          match property {
            oast::AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(property) => {
              if let Some(error) =
                Self::strict_binding_name_error(self.strict_context, property.binding.name.as_str())
              {
                return Err(error);
              }
              if let Some(initializer) = &property.init {
                self.validate_early_errors_in_expression(initializer)?;
              }
            }
            oast::AssignmentTargetProperty::AssignmentTargetPropertyProperty(property) => {
              if property.computed
                && let Some(key) = property.name.as_expression()
              {
                self.validate_early_errors_in_expression(key)?;
              }
              self.validate_assignment_target_maybe_default(&property.binding)?;
            }
          }
        }
        if let Some(rest) = &object.rest {
          self.validate_assignment_target(&rest.target)?;
        }
        Ok(())
      }
      _ => Ok(()),
    }
  }

  pub(super) fn validate_assignment_target_maybe_default(
    &self,
    target: &oast::AssignmentTargetMaybeDefault<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    match target {
      oast::AssignmentTargetMaybeDefault::AssignmentTargetWithDefault(default) => {
        self.validate_assignment_target(&default.binding)?;
        self.validate_early_errors_in_expression(&default.init)
      }
      oast::AssignmentTargetMaybeDefault::AssignmentTargetIdentifier(identifier) => {
        if let Some(error) =
          Self::strict_binding_name_error(self.strict_context, identifier.name.as_str())
        {
          return Err(error);
        }
        Ok(())
      }
      oast::AssignmentTargetMaybeDefault::StaticMemberExpression(member) => {
        self.validate_early_errors_in_expression(&member.object)
      }
      oast::AssignmentTargetMaybeDefault::ComputedMemberExpression(member) => {
        self.validate_early_errors_in_expression(&member.object)?;
        self.validate_early_errors_in_expression(&member.expression)
      }
      oast::AssignmentTargetMaybeDefault::ArrayAssignmentTarget(array) => {
        for element in array.elements.iter().flatten() {
          self.validate_assignment_target_maybe_default(element)?;
        }
        if let Some(rest) = &array.rest {
          self.validate_assignment_target(&rest.target)?;
        }
        Ok(())
      }
      oast::AssignmentTargetMaybeDefault::ObjectAssignmentTarget(object) => {
        for property in &object.properties {
          match property {
            oast::AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(property) => {
              if let Some(error) =
                Self::strict_binding_name_error(self.strict_context, property.binding.name.as_str())
              {
                return Err(error);
              }
              if let Some(initializer) = &property.init {
                self.validate_early_errors_in_expression(initializer)?;
              }
            }
            oast::AssignmentTargetProperty::AssignmentTargetPropertyProperty(property) => {
              if property.computed
                && let Some(key) = property.name.as_expression()
              {
                self.validate_early_errors_in_expression(key)?;
              }
              self.validate_assignment_target_maybe_default(&property.binding)?;
            }
          }
        }
        if let Some(rest) = &object.rest {
          self.validate_assignment_target(&rest.target)?;
        }
        Ok(())
      }
      _ => Ok(()),
    }
  }

  pub(super) fn validate_for_statement_left(
    &self,
    left: &oast::ForStatementLeft<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    match left {
      oast::ForStatementLeft::VariableDeclaration(declaration) => {
        for declarator in &declaration.declarations {
          if let Some(error) =
            Self::strict_binding_pattern_name_error(self.strict_context, &declarator.id)
          {
            return Err(error);
          }
          if let Some(initializer) = &declarator.init {
            self.validate_early_errors_in_expression(initializer)?;
          }
        }
        Ok(())
      }
      oast::ForStatementLeft::AssignmentTargetIdentifier(identifier) => {
        if let Some(error) =
          Self::strict_binding_name_error(self.strict_context, identifier.name.as_str())
        {
          return Err(error);
        }
        Ok(())
      }
      oast::ForStatementLeft::StaticMemberExpression(member) => {
        self.validate_early_errors_in_expression(&member.object)
      }
      oast::ForStatementLeft::ComputedMemberExpression(member) => {
        self.validate_early_errors_in_expression(&member.object)?;
        self.validate_early_errors_in_expression(&member.expression)
      }
      oast::ForStatementLeft::ArrayAssignmentTarget(array) => {
        for element in array.elements.iter().flatten() {
          self.validate_assignment_target_maybe_default(element)?;
        }
        if let Some(rest) = &array.rest {
          self.validate_assignment_target(&rest.target)?;
        }
        Ok(())
      }
      oast::ForStatementLeft::ObjectAssignmentTarget(object) => {
        for property in &object.properties {
          match property {
            oast::AssignmentTargetProperty::AssignmentTargetPropertyIdentifier(property) => {
              if let Some(error) =
                Self::strict_binding_name_error(self.strict_context, property.binding.name.as_str())
              {
                return Err(error);
              }
              if let Some(initializer) = &property.init {
                self.validate_early_errors_in_expression(initializer)?;
              }
            }
            oast::AssignmentTargetProperty::AssignmentTargetPropertyProperty(property) => {
              if property.computed
                && let Some(key) = property.name.as_expression()
              {
                self.validate_early_errors_in_expression(key)?;
              }
              self.validate_assignment_target_maybe_default(&property.binding)?;
            }
          }
        }
        if let Some(rest) = &object.rest {
          self.validate_assignment_target(&rest.target)?;
        }
        Ok(())
      }
      _ => Ok(()),
    }
  }

  pub(super) fn parameter_expression_contains_yield(expression: &oast::Expression<'_>) -> bool {
    match expression {
      oast::Expression::YieldExpression(_) => true,
      oast::Expression::ParenthesizedExpression(parenthesized) => {
        Self::parameter_expression_contains_yield(&parenthesized.expression)
      }
      oast::Expression::SequenceExpression(sequence) => sequence
        .expressions
        .iter()
        .any(Self::parameter_expression_contains_yield),
      oast::Expression::ConditionalExpression(conditional) => {
        Self::parameter_expression_contains_yield(&conditional.test)
          || Self::parameter_expression_contains_yield(&conditional.consequent)
          || Self::parameter_expression_contains_yield(&conditional.alternate)
      }
      oast::Expression::BinaryExpression(binary) => {
        Self::parameter_expression_contains_yield(&binary.left)
          || Self::parameter_expression_contains_yield(&binary.right)
      }
      oast::Expression::LogicalExpression(logical) => {
        Self::parameter_expression_contains_yield(&logical.left)
          || Self::parameter_expression_contains_yield(&logical.right)
      }
      oast::Expression::UnaryExpression(unary) => {
        Self::parameter_expression_contains_yield(&unary.argument)
      }
      _ => false,
    }
  }

  pub(super) fn validate_function_with_context(
    &self,
    function: &oast::Function<'_>,
    force_strict: bool,
  ) -> Result<(), crate::errors::CompilerError> {
    let mut params = HashSet::new();
    let simple = function.params.rest.is_none()
      && function.params.items.iter().all(|p| {
        p.initializer.is_none() && matches!(p.pattern, oast::BindingPattern::BindingIdentifier(_))
      });
    for parameter in &function.params.items {
      let mut names = HashSet::new();
      Self::collect_binding_names(&parameter.pattern, &mut names);
      let duplicate = names.iter().any(|name| params.contains(name));
      if !simple && duplicate {
        return Err(crate::errors::CompilerError::from(
          "SyntaxError: duplicate parameter name",
        ));
      }
      params.extend(names);
      if let Some(init) = &parameter.initializer {
        if (force_strict || function.generator) && Self::parameter_expression_contains_yield(init) {
          return Err(crate::errors::CompilerError::from(
            "SyntaxError: yield is not allowed in function parameters",
          ));
        }
        self.validate_early_errors_in_expression(init)?;
      }
    }
    if let Some(rest) = &function.params.rest {
      let mut names = HashSet::new();
      Self::collect_binding_names(&rest.rest.argument, &mut names);
      if names
        .iter()
        .any(|name| !params.insert(name.clone()))
      {
        return Err(crate::errors::CompilerError::from(
          "SyntaxError: duplicate parameter name",
        ));
      }
    }
    if let Some(body) = &function.body {
      let mut lexical = HashSet::new();
      for stmt in &body.statements {
        Self::lexical_declared_names_of_statement(stmt, &mut lexical, true);
      }
      if params.iter().any(|name| lexical.contains(name)) {
        return Err(crate::errors::CompilerError::from(
          "SyntaxError: parameter/body lexical conflict",
        ));
      }
      self.validate_early_errors_in_statements(&body.statements, false, true)?;
    }
    Ok(())
  }

  pub(super) fn validate_function(
    &self,
    function: &oast::Function<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    self.validate_function_with_context(function, false)
  }

  pub(super) fn expression_contains_direct_super_call(expression: &oast::Expression<'_>) -> bool {
    match expression {
      oast::Expression::CallExpression(call) => {
        matches!(call.callee, oast::Expression::Super(_))
          || Self::expression_contains_direct_super_call(&call.callee)
          || call.arguments.iter().any(|argument| {
            argument
              .as_expression()
              .is_some_and(Self::expression_contains_direct_super_call)
          })
      }
      oast::Expression::ParenthesizedExpression(parenthesized) => {
        Self::expression_contains_direct_super_call(&parenthesized.expression)
      }
      oast::Expression::SequenceExpression(sequence) => sequence
        .expressions
        .iter()
        .any(Self::expression_contains_direct_super_call),
      oast::Expression::ConditionalExpression(conditional) => {
        Self::expression_contains_direct_super_call(&conditional.test)
          || Self::expression_contains_direct_super_call(&conditional.consequent)
          || Self::expression_contains_direct_super_call(&conditional.alternate)
      }
      _ => false,
    }
  }

  pub(crate) fn statements_contain_direct_super_call(statements: &[oast::Statement<'_>]) -> bool {
    statements
      .iter()
      .any(|statement| match statement {
        oast::Statement::ExpressionStatement(expression) => {
          Self::expression_contains_direct_super_call(&expression.expression)
        }
        oast::Statement::ReturnStatement(statement) => statement
          .argument
          .as_ref()
          .is_some_and(Self::expression_contains_direct_super_call),
        oast::Statement::ThrowStatement(statement) => {
          Self::expression_contains_direct_super_call(&statement.argument)
        }
        oast::Statement::BlockStatement(block) => {
          Self::statements_contain_direct_super_call(&block.body)
        }
        _ => false,
      })
  }

  pub(super) fn validate_class(
    &self,
    class: &oast::Class<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    if let Some(super_class) = &class.super_class {
      self.validate_early_errors_in_expression(super_class)?;
    }
    let constructor_count = class
      .body
      .body
      .iter()
      .filter(|element| {
        matches!(
          element,
          oast::ClassElement::MethodDefinition(method)
            if matches!(method.kind, oast::MethodDefinitionKind::Constructor)
        )
      })
      .count();
    if constructor_count > 1 {
      return Err(crate::errors::CompilerError::from(
        "SyntaxError: class may only have one constructor",
      ));
    }
    for element in &class.body.body {
      match element {
        oast::ClassElement::StaticBlock(block) => {
          self.validate_early_errors_in_statements(&block.body, false, false)?;
        }
        oast::ClassElement::MethodDefinition(method) => {
          if method.computed
            && let Some(key) = method.key.as_expression()
          {
            self.validate_early_errors_in_expression(key)?;
          }
          if !matches!(method.kind, oast::MethodDefinitionKind::Constructor)
            && method
              .value
              .body
              .as_ref()
              .is_some_and(|body| Self::statements_contain_direct_super_call(&body.statements))
          {
            return Err(crate::errors::CompilerError::from(
              "SyntaxError: direct super call is only valid in a constructor",
            ));
          }
          self.validate_function_with_context(&method.value, true)?;
        }
        oast::ClassElement::PropertyDefinition(property) => {
          if property.computed
            && let Some(key) = property.key.as_expression()
          {
            self.validate_early_errors_in_expression(key)?;
          }
          if let Some(value) = &property.value {
            self.validate_early_errors_in_expression(value)?;
          }
        }
        oast::ClassElement::AccessorProperty(property) => {
          if let Some(value) = &property.value {
            self.validate_early_errors_in_expression(value)?;
          }
        }
        oast::ClassElement::TSIndexSignature(_) => {}
      }
    }
    Ok(())
  }

  pub(super) fn validate_early_errors_in_expression(
    &self,
    expr: &oast::Expression<'_>,
  ) -> Result<(), crate::errors::CompilerError> {
    use oast::Expression as E;
    match expr {
      E::Identifier(identifier) => {
        if let Some(error) =
          Self::strict_identifier_reference_error(self.strict_context, identifier.name.as_str())
        {
          return Err(error);
        }
        Ok(())
      }
      E::FunctionExpression(f) => self.validate_function(f),
      E::ArrowFunctionExpression(a) => {
        for parameter in &a.params.items {
          if let Some(initializer) = &parameter.initializer {
            if Self::parameter_expression_contains_yield(initializer) {
              return Err(crate::errors::CompilerError::from(
                "SyntaxError: yield is not allowed in arrow parameters",
              ));
            }
            self.validate_early_errors_in_expression(initializer)?;
          }
        }
        self.validate_early_errors_in_statements(&a.body.statements, false, true)
      }
      E::ClassExpression(c) => self.validate_class(c),
      E::ParenthesizedExpression(p) => self.validate_early_errors_in_expression(&p.expression),
      E::ArrayExpression(a) => {
        for element in &a.elements {
          if let Some(e) = element.as_expression() {
            self.validate_early_errors_in_expression(e)?;
          } else if let oast::ArrayExpressionElement::SpreadElement(s) = element {
            self.validate_early_errors_in_expression(&s.argument)?;
          }
        }
        Ok(())
      }
      E::ObjectExpression(o) => {
        for property in &o.properties {
          match property {
            oast::ObjectPropertyKind::ObjectProperty(p) => {
              if p.computed
                && let Some(key) = p.key.as_expression()
              {
                self.validate_early_errors_in_expression(key)?;
              }
              if p.method
                && let oast::Expression::FunctionExpression(function) = &p.value
              {
                let parameter_has_direct_super = function.params.items.iter().any(|parameter| {
                  parameter
                    .initializer
                    .as_ref()
                    .is_some_and(|initializer| {
                      Self::expression_contains_direct_super_call(initializer)
                    })
                });
                let body_has_direct_super = function
                  .body
                  .as_ref()
                  .is_some_and(|body| Self::statements_contain_direct_super_call(&body.statements));
                if parameter_has_direct_super || body_has_direct_super {
                  return Err(crate::errors::CompilerError::from(
                    "SyntaxError: direct super call is only valid in a constructor",
                  ));
                }
              }
              self.validate_early_errors_in_expression(&p.value)?;
            }
            oast::ObjectPropertyKind::SpreadProperty(s) => {
              self.validate_early_errors_in_expression(&s.argument)?;
            }
          }
        }
        Ok(())
      }
      E::TemplateLiteral(t) => {
        for e in &t.expressions {
          self.validate_early_errors_in_expression(e)?;
        }
        Ok(())
      }
      E::TaggedTemplateExpression(t) => {
        self.validate_early_errors_in_expression(&t.tag)?;
        for e in &t.quasi.expressions {
          self.validate_early_errors_in_expression(e)?;
        }
        Ok(())
      }
      E::CallExpression(c) => {
        self.validate_early_errors_in_expression(&c.callee)?;
        for a in &c.arguments {
          if let Some(e) = a.as_expression() {
            self.validate_early_errors_in_expression(e)?;
          } else if let oast::Argument::SpreadElement(s) = a {
            self.validate_early_errors_in_expression(&s.argument)?;
          }
        }
        Ok(())
      }
      E::NewExpression(n) => {
        self.validate_early_errors_in_expression(&n.callee)?;
        for a in &n.arguments {
          if let Some(e) = a.as_expression() {
            self.validate_early_errors_in_expression(e)?;
          } else if let oast::Argument::SpreadElement(s) = a {
            self.validate_early_errors_in_expression(&s.argument)?;
          }
        }
        Ok(())
      }
      E::ComputedMemberExpression(m) => {
        self.validate_early_errors_in_expression(&m.object)?;
        self.validate_early_errors_in_expression(&m.expression)
      }
      E::StaticMemberExpression(m) => self.validate_early_errors_in_expression(&m.object),
      E::PrivateFieldExpression(m) => self.validate_early_errors_in_expression(&m.object),
      E::ChainExpression(c) => match &c.expression {
        oast::ChainElement::CallExpression(call) => {
          self.validate_early_errors_in_expression(&call.callee)?;
          for a in &call.arguments {
            if let Some(e) = a.as_expression() {
              self.validate_early_errors_in_expression(e)?;
            } else if let oast::Argument::SpreadElement(s) = a {
              self.validate_early_errors_in_expression(&s.argument)?;
            }
          }
          Ok(())
        }
        oast::ChainElement::TSNonNullExpression(e) => {
          self.validate_early_errors_in_expression(&e.expression)
        }
        oast::ChainElement::ComputedMemberExpression(m) => {
          self.validate_early_errors_in_expression(&m.object)?;
          self.validate_early_errors_in_expression(&m.expression)
        }
        oast::ChainElement::StaticMemberExpression(m) => {
          self.validate_early_errors_in_expression(&m.object)
        }
        oast::ChainElement::PrivateFieldExpression(m) => {
          self.validate_early_errors_in_expression(&m.object)
        }
      },
      E::AssignmentExpression(a) => {
        self.validate_assignment_target(&a.left)?;
        self.validate_early_errors_in_expression(&a.right)
      }
      E::AwaitExpression(a) => self.validate_early_errors_in_expression(&a.argument),
      E::BinaryExpression(a) => {
        self.validate_early_errors_in_expression(&a.left)?;
        self.validate_early_errors_in_expression(&a.right)
      }
      E::LogicalExpression(a) => {
        self.validate_early_errors_in_expression(&a.left)?;
        self.validate_early_errors_in_expression(&a.right)
      }
      E::ConditionalExpression(a) => {
        self.validate_early_errors_in_expression(&a.test)?;
        self.validate_early_errors_in_expression(&a.consequent)?;
        self.validate_early_errors_in_expression(&a.alternate)
      }
      E::SequenceExpression(a) => {
        for e in &a.expressions {
          self.validate_early_errors_in_expression(e)?;
        }
        Ok(())
      }
      E::UnaryExpression(a) => self.validate_early_errors_in_expression(&a.argument),
      E::YieldExpression(a) => {
        if let Some(e) = &a.argument {
          self.validate_early_errors_in_expression(e)?;
        }
        Ok(())
      }
      E::PrivateInExpression(a) => self.validate_early_errors_in_expression(&a.right),
      E::TSAsExpression(a) => self.validate_early_errors_in_expression(&a.expression),
      E::TSSatisfiesExpression(a) => self.validate_early_errors_in_expression(&a.expression),
      E::TSTypeAssertion(a) => self.validate_early_errors_in_expression(&a.expression),
      E::TSNonNullExpression(a) => self.validate_early_errors_in_expression(&a.expression),
      E::TSInstantiationExpression(a) => self.validate_early_errors_in_expression(&a.expression),
      _ => Ok(()),
    }
  }

  pub(super) fn validate_early_errors_in_program(
    &self,
    stmts: &[oast::Statement<'_>],
    allow_module_declarations: bool,
  ) -> Result<(), crate::errors::CompilerError> {
    self.validate_early_errors_in_statements(
      stmts,
      allow_module_declarations,
      !allow_module_declarations,
    )
  }

  pub(super) fn validate_early_errors_in_statements(
    &self,
    stmts: &[oast::Statement<'_>],
    allow_module_declarations: bool,
    function_body: bool,
  ) -> Result<(), crate::errors::CompilerError> {
    let mut lexical = HashSet::new();
    let mut vars = HashSet::new();
    for stmt in stmts {
      Self::lexical_declared_names_of_statement(stmt, &mut lexical, function_body);
      if function_body
        && let oast::Statement::FunctionDeclaration(function) = stmt
        && let Some(id) = &function.id
      {
        vars.insert(id.name.to_string());
      }
      self.var_declared_names_of_statement(stmt, &mut vars);
    }
    let mut seen_lexical = HashSet::new();
    for stmt in stmts {
      let mut names = HashSet::new();
      Self::lexical_declared_names_of_statement(stmt, &mut names, function_body);
      let mut names: Vec<_> = names.into_iter().collect();
      names.sort();
      for name in names {
        if !seen_lexical.insert(name.clone()) {
          return Err(crate::errors::CompilerError::from(format!(
            "SyntaxError: redeclaration of lexical binding `{}`",
            name
          )));
        }
      }
    }
    if let Some(name) = lexical.iter().find(|name| vars.contains(*name)) {
      return Err(crate::errors::CompilerError::from(format!(
        "SyntaxError: redeclaration of lexical binding `{}`",
        name
      )));
    }

    for stmt in stmts {
      if matches!(
        stmt,
        oast::Statement::ImportDeclaration(_)
          | oast::Statement::ExportAllDeclaration(_)
          | oast::Statement::ExportDefaultDeclaration(_)
          | oast::Statement::ExportNamedDeclaration(_)
      ) && !allow_module_declarations
      {
        return Err(crate::errors::CompilerError::from(
          "SyntaxError: module declaration in statement position",
        ));
      }
      match stmt {
        oast::Statement::BlockStatement(b) => {
          self.validate_early_errors_in_statements(&b.body, false, false)?
        }
        oast::Statement::IfStatement(i) => {
          self.validate_early_errors_in_expression(&i.test)?;
          self.validate_statement_position_function(&i.consequent)?;
          self.validate_early_errors_in_statements(
            std::slice::from_ref(&i.consequent),
            false,
            false,
          )?;
          if let Some(alt) = &i.alternate {
            self.validate_statement_position_function(alt)?;
            self.validate_early_errors_in_statements(std::slice::from_ref(alt), false, false)?;
          }
        }
        oast::Statement::WhileStatement(w) => {
          self.validate_early_errors_in_expression(&w.test)?;
          self.validate_statement_position_function(&w.body)?;
          self.validate_early_errors_in_statements(std::slice::from_ref(&w.body), false, false)?
        }
        oast::Statement::DoWhileStatement(d) => {
          self.validate_early_errors_in_expression(&d.test)?;
          self.validate_statement_position_function(&d.body)?;
          self.validate_early_errors_in_statements(std::slice::from_ref(&d.body), false, false)?
        }
        oast::Statement::ForStatement(f) => {
          self.validate_statement_position_function(&f.body)?;
          self.validate_early_errors_in_statements(std::slice::from_ref(&f.body), false, false)?
        }
        oast::Statement::ForInStatement(f) => {
          self.validate_for_statement_left(&f.left)?;
          self.validate_early_errors_in_expression(&f.right)?;
          self.validate_statement_position_function(&f.body)?;
          self.validate_early_errors_in_statements(std::slice::from_ref(&f.body), false, false)?
        }
        oast::Statement::ForOfStatement(f) => {
          self.validate_for_statement_left(&f.left)?;
          self.validate_early_errors_in_expression(&f.right)?;
          self.validate_statement_position_function(&f.body)?;
          self.validate_early_errors_in_statements(std::slice::from_ref(&f.body), false, false)?
        }
        oast::Statement::LabeledStatement(l) => {
          self.validate_statement_position_function(&l.body)?;
          self.validate_early_errors_in_statements(std::slice::from_ref(&l.body), false, false)?;
        }
        oast::Statement::WithStatement(w) => {
          self.validate_early_errors_in_statements(std::slice::from_ref(&w.body), false, false)?
        }
        oast::Statement::SwitchStatement(s) => {
          self.validate_early_errors_in_expression(&s.discriminant)?;
          let mut lexical = HashSet::new();
          let mut vars = HashSet::new();
          for case in &s.cases {
            if let Some(test) = &case.test {
              self.validate_early_errors_in_expression(test)?;
            }
            for statement in &case.consequent {
              let mut names = HashSet::new();
              Self::lexical_declared_names_of_statement(statement, &mut names, false);
              for name in names {
                if !lexical.insert(name.clone()) {
                  return Err(crate::errors::CompilerError::from(format!(
                    "SyntaxError: redeclaration of lexical binding `{name}`"
                  )));
                }
              }
              self.var_declared_names_of_statement(statement, &mut vars);
            }
          }
          if let Some(name) = lexical.iter().find(|name| vars.contains(*name)) {
            return Err(crate::errors::CompilerError::from(format!(
              "SyntaxError: redeclaration of lexical binding `{name}`"
            )));
          }
          for case in &s.cases {
            self.validate_early_errors_in_statements(&case.consequent, false, false)?;
          }
        }
        oast::Statement::TryStatement(t) => {
          self.validate_early_errors_in_statements(&t.block.body, false, false)?;
          if let Some(handler) = &t.handler {
            if let Some(parameter) = &handler.param {
              let mut names = HashSet::new();
              Self::collect_binding_names(&parameter.pattern, &mut names);
              let mut lexical = HashSet::new();
              for stmt in &handler.body.body {
                Self::lexical_declared_names_of_statement(stmt, &mut lexical, false);
              }
              if names.iter().any(|name| lexical.contains(name)) {
                return Err(crate::errors::CompilerError::from(
                  "SyntaxError: catch parameter/body lexical conflict",
                ));
              }
            }
            self.validate_early_errors_in_statements(&handler.body.body, false, false)?;
          }
          if let Some(finalizer) = &t.finalizer {
            self.validate_early_errors_in_statements(&finalizer.body, false, false)?;
          }
        }
        oast::Statement::FunctionDeclaration(f) => {
          self.validate_function(f)?;
        }
        oast::Statement::ClassDeclaration(c) => {
          self.validate_class(c)?;
        }
        oast::Statement::VariableDeclaration(v) => {
          self.validate_variable_initializers(v)?;
        }
        oast::Statement::ExpressionStatement(e) => {
          self.validate_early_errors_in_expression(&e.expression)?;
        }
        oast::Statement::ReturnStatement(s) => {
          if let Some(expression) = &s.argument {
            self.validate_early_errors_in_expression(expression)?;
          }
        }
        oast::Statement::ThrowStatement(s) => {
          self.validate_early_errors_in_expression(&s.argument)?;
        }
        _ => {}
      }
    }
    Ok(())
  }
}
