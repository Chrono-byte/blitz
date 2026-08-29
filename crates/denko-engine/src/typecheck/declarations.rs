// SPDX-License-Identifier: MPL-2.0

use super::*;

impl TypeContext {
  pub(super) fn register_declarations_sources(
    &mut self,
    declarations: &[VirtualDeclaration],
  ) -> Result<(), TypeCheckerError> {
    let declaration_type =
      crate::parser::Parser::source_type_for_path(std::path::Path::new("virtual.d.ts"), false);
    let source_type =
      crate::parser::Parser::source_type_for_path(std::path::Path::new("virtual.ts"), true);
    let mut parsed = Vec::with_capacity(declarations.len());
    for declaration in declarations {
      let session =
        crate::parser::Parser::parse_source_with_type(&declaration.source, declaration_type)
          .or_else(|_| {
            crate::parser::Parser::parse_source_with_type(&declaration.source, source_type)
          })
          .map_err(|error| {
            TypeCheckerError::from(format!(
              "Failed to parse declaration for '{}': {}",
              declaration.specifier, error
            ))
          })?;
      parsed.push(ParsedDeclaration {
        specifier: declaration.specifier.clone(),
        session,
      });
    }
    parsed.sort_by(|left, right| left.specifier.cmp(&right.specifier));
    if parsed
      .windows(2)
      .any(|pair| pair[0].specifier == pair[1].specifier)
    {
      return Err(TypeCheckerError::from("Duplicate declaration module"));
    }

    for declaration in &parsed {
      let mut local = ModuleType::default();
      let mut exported_values = HashSet::new();
      let mut exported_types = HashSet::new();
      let mut export_aliases = Vec::new();
      for statement in &declaration.session.program().body {
        match statement {
          oast::Statement::ExportNamedDeclaration(export) => {
            if let Some(declaration) = &export.declaration {
              self.register_module_declaration(declaration, &mut local)?;
              module_declaration_names(declaration, &mut exported_values, &mut exported_types);
            } else if export.source.is_none() {
              for specifier in &export.specifiers {
                export_aliases.push((
                  module_export_name(&specifier.local)?,
                  module_export_name(&specifier.exported)?,
                ));
              }
            }
          }
          oast::Statement::TSTypeAliasDeclaration(_)
          | oast::Statement::TSInterfaceDeclaration(_)
          | oast::Statement::VariableDeclaration(_)
          | oast::Statement::FunctionDeclaration(_) => {
            self.register_module_statement_declaration(statement, &mut local)?;
          }
          _ => {}
        }
      }

      let local_type_names: HashSet<String> = local.types.keys().cloned().collect();
      let mut qualified_types = HashMap::new();
      for (name, definition) in local.types {
        let definition =
          qualify_type_definition(&declaration.specifier, &local_type_names, &definition);
        self.named_types.insert(
          private_type_name(&declaration.specifier, &name),
          definition.clone(),
        );
        qualified_types.insert(name, definition);
      }
      let qualified_values = local
        .values
        .into_iter()
        .map(|(name, typ)| {
          let qualified = qualify_type(
            &declaration.specifier,
            &local_type_names,
            &HashSet::new(),
            &typ,
          );
          self.copy_function_parameter_metadata(&typ, &qualified);
          (name, qualified)
        })
        .collect::<HashMap<_, _>>();
      let mut public_values = qualified_values
        .iter()
        .filter(|(name, _)| exported_values.contains(*name))
        .map(|(name, typ)| (name.clone(), typ.clone()))
        .collect::<HashMap<_, _>>();
      let mut public_types = qualified_types
        .iter()
        .filter(|(name, _)| exported_types.contains(*name))
        .map(|(name, definition)| (name.clone(), definition.clone()))
        .collect::<HashMap<_, _>>();
      for (local_name, exported_name) in export_aliases {
        if let Some(definition) = qualified_types.get(&local_name) {
          public_types.insert(exported_name, definition.clone());
        } else if let Some(typ) = qualified_values.get(&local_name) {
          public_values.insert(exported_name, typ.clone());
        }
      }
      let module = ModuleType {
        values: public_values,
        types: public_types,
        namespaces: HashMap::new(),
      };
      self
        .modules
        .insert(declaration.specifier.clone(), module);
    }

    for _ in 0..=parsed.len() {
      for declaration in &parsed {
        let mut module = self
          .modules
          .get(&declaration.specifier)
          .cloned()
          .expect("direct declaration module was inserted");
        for statement in &declaration.session.program().body {
          self.register_module_links(statement, &mut module)?;
        }
        self
          .modules
          .insert(declaration.specifier.clone(), module);
      }
    }

    for declaration in &parsed {
      let mut module = self
        .modules
        .get(&declaration.specifier)
        .cloned()
        .expect("linked declaration module was inserted");
      for statement in &declaration.session.program().body {
        self.register_module_links_strict(statement, &mut module)?;
      }
      self
        .modules
        .insert(declaration.specifier.clone(), module);
    }
    Ok(())
  }

  pub(super) fn register_module_links(
    &mut self,
    statement: &oast::Statement<'_>,
    module: &mut ModuleType,
  ) -> Result<(), TypeCheckerError> {
    match statement {
      oast::Statement::ImportDeclaration(import) => {
        self.register_module_import(import, false)?;
      }
      oast::Statement::ExportNamedDeclaration(export) => {
        if let Some(source) = &export.source {
          self.register_reexport(source.value.as_str(), &export.specifiers, module, false)?;
        }
      }
      oast::Statement::ExportAllDeclaration(export) => {
        self.register_export_all(export, module)?;
      }
      _ => {}
    }
    Ok(())
  }

  pub(super) fn register_module_links_strict(
    &mut self,
    statement: &oast::Statement<'_>,
    module: &mut ModuleType,
  ) -> Result<(), TypeCheckerError> {
    match statement {
      oast::Statement::ImportDeclaration(import) => {
        self.register_module_import(import, true)?;
      }
      oast::Statement::ExportNamedDeclaration(export) => {
        if let Some(source) = &export.source {
          self.register_reexport(source.value.as_str(), &export.specifiers, module, true)?;
        }
      }
      oast::Statement::ExportAllDeclaration(export) => {
        self.register_export_all(export, module)?;
      }
      _ => {}
    }
    Ok(())
  }

  pub(super) fn register_module_import(
    &mut self,
    import: &oast::ImportDeclaration<'_>,
    strict: bool,
  ) -> Result<(), TypeCheckerError> {
    let source = import.source.value.as_str();
    let module = self.module(source)?.clone();
    let Some(specifiers) = &import.specifiers else {
      return Ok(());
    };
    for specifier in specifiers {
      match specifier {
        oast::ImportDeclarationSpecifier::ImportNamespaceSpecifier(namespace) => {
          register_namespace_types(
            namespace.local.name.as_str(),
            &module,
            &mut self.namespace_types,
          );
        }
        oast::ImportDeclarationSpecifier::ImportDefaultSpecifier(_) => {}
        oast::ImportDeclarationSpecifier::ImportSpecifier(named) => {
          let imported = module_export_name(&named.imported)?;
          if let Some(definition) = module.types.get(&imported) {
            self
              .named_types
              .insert(named.local.name.to_string(), definition.clone());
          } else if !module.values.contains_key(&imported) && strict {
            return Err(TypeCheckerError::from(format!(
              "Module '{source}' has no export '{imported}'"
            )));
          }
        }
      }
    }
    Ok(())
  }

  pub(super) fn register_reexport(
    &mut self,
    source: &str,
    specifiers: &[oast::ExportSpecifier<'_>],
    module: &mut ModuleType,
    strict: bool,
  ) -> Result<(), TypeCheckerError> {
    let imported = self.module(source)?.clone();
    for specifier in specifiers {
      let local = module_export_name(&specifier.local)?;
      let exported = module_export_name(&specifier.exported)?;
      if let Some(typ) = imported.values.get(&local) {
        module.values.insert(exported, typ.clone());
      } else if let Some(definition) = imported.types.get(&local) {
        module.types.insert(exported, definition.clone());
      } else if strict {
        return Err(TypeCheckerError::from(format!(
          "Module '{source}' has no export '{local}'"
        )));
      }
    }
    Ok(())
  }

  pub(super) fn register_export_all(
    &mut self,
    export: &oast::ExportAllDeclaration<'_>,
    module: &mut ModuleType,
  ) -> Result<(), TypeCheckerError> {
    let imported = self.module(export.source.value.as_str())?.clone();
    if let Some(exported) = &export.exported {
      module
        .namespaces
        .insert(module_export_name(exported)?, imported);
    } else if export.export_kind == oast::ImportOrExportKind::Type {
      module.types.extend(imported.types);
      module.namespaces.extend(imported.namespaces);
    } else {
      module.values.extend(imported.values);
      module.types.extend(imported.types);
      module.namespaces.extend(imported.namespaces);
    }
    Ok(())
  }

  pub(super) fn module(
    &self,
    source: &str,
  ) -> Result<&ModuleType, TypeCheckerError> {
    self
      .modules
      .get(source)
      .ok_or_else(|| TypeCheckerError::from(format!("Unknown declaration module '{source}'")))
  }

  pub(super) fn register_module_statement_declaration(
    &mut self,
    statement: &oast::Statement<'_>,
    module: &mut ModuleType,
  ) -> Result<(), TypeCheckerError> {
    match statement {
      oast::Statement::TSTypeAliasDeclaration(alias) => {
        module.types.insert(
          alias.id.name.to_string(),
          type_definition(alias.type_parameters.as_deref(), &alias.type_annotation)?,
        );
      }
      oast::Statement::TSInterfaceDeclaration(interface) => {
        module.types.insert(
          interface.id.name.to_string(),
          TypeDefinition {
            parameters: convert_type_parameters(interface.type_parameters.as_deref())?,
            body: self.interface_type(interface)?,
          },
        );
      }
      oast::Statement::VariableDeclaration(variable) => {
        for declarator in &variable.declarations {
          let oast::BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
            return Err(TypeCheckerError::from(
              "unsupported declaration binding pattern",
            ));
          };
          let typ = self.annotated_type(declarator.type_annotation.as_deref(), Type::Unknown)?;
          module
            .values
            .insert(identifier.name.to_string(), typ);
        }
      }
      oast::Statement::FunctionDeclaration(function) => {
        if let Some(identifier) = &function.id {
          let typ = self.function_type(&function.params, function.return_type.as_deref())?;
          merge_callable(&mut module.values, identifier.name.as_str(), typ);
        }
      }
      _ => {}
    }
    Ok(())
  }

  pub(super) fn register_module_declaration(
    &mut self,
    declaration: &oast::Declaration<'_>,
    module: &mut ModuleType,
  ) -> Result<(), TypeCheckerError> {
    match declaration {
      oast::Declaration::TSTypeAliasDeclaration(alias) => {
        module.types.insert(
          alias.id.name.to_string(),
          type_definition(alias.type_parameters.as_deref(), &alias.type_annotation)?,
        );
      }
      oast::Declaration::TSInterfaceDeclaration(interface) => {
        module.types.insert(
          interface.id.name.to_string(),
          TypeDefinition {
            parameters: convert_type_parameters(interface.type_parameters.as_deref())?,
            body: self.interface_type(interface)?,
          },
        );
      }
      oast::Declaration::VariableDeclaration(variable) => {
        for declarator in &variable.declarations {
          let oast::BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
            return Err(TypeCheckerError::from(
              "unsupported declaration binding pattern",
            ));
          };
          let typ = self.annotated_type(declarator.type_annotation.as_deref(), Type::Unknown)?;
          module
            .values
            .insert(identifier.name.to_string(), typ);
        }
      }
      oast::Declaration::FunctionDeclaration(function) => {
        if let Some(identifier) = &function.id {
          let typ = self.function_type(&function.params, function.return_type.as_deref())?;
          merge_callable(&mut module.values, identifier.name.as_str(), typ);
        }
      }
      _ => {}
    }
    Ok(())
  }

  pub(super) fn scope_mut(&mut self) -> &mut Scope {
    self
      .scopes
      .last_mut()
      .expect("scope stack is empty: attempted to borrow mutable scope but no scopes are present")
  }

  pub(super) fn lookup(
    &self,
    name: &str,
  ) -> Option<Type> {
    for s in self.scopes.iter().rev() {
      if let Some(t) = s.get(name) {
        return Some(t.clone());
      }
    }
    None
  }

  pub(super) fn register_declarations(
    &mut self,
    stmt: &oast::Statement<'_>,
  ) -> Result<(), TypeCheckerError> {
    match stmt {
      oast::Statement::TSTypeAliasDeclaration(alias) => {
        self.named_types.insert(
          alias.id.name.to_string(),
          type_definition(alias.type_parameters.as_deref(), &alias.type_annotation)?,
        );
      }
      oast::Statement::TSInterfaceDeclaration(interface) => {
        let typ = self.interface_type(interface)?;
        self.named_types.insert(
          interface.id.name.to_string(),
          TypeDefinition {
            parameters: convert_type_parameters(interface.type_parameters.as_deref())?,
            body: typ,
          },
        );
      }
      oast::Statement::FunctionDeclaration(function) => {
        if let Some(id) = &function.id {
          let typ = self.function_type(&function.params, function.return_type.as_deref())?;
          merge_callable_scope(self.scope_mut(), id.name.as_str(), typ);
        }
      }
      oast::Statement::VariableDeclaration(variable) => {
        for declarator in &variable.declarations {
          let oast::BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
            return Err(TypeCheckerError::from(
              "unsupported variable binding pattern",
            ));
          };
          let typ = self.annotated_type(declarator.type_annotation.as_deref(), Type::Unknown)?;
          self
            .scope_mut()
            .insert(identifier.name.to_string(), typ);
        }
      }
      oast::Statement::ImportDeclaration(import) => {
        if self.check_imports {
          self.register_import(import)?;
        } else {
          self.register_source_import(import, Type::Any);
        }
      }
      oast::Statement::ExportNamedDeclaration(export) => {
        if let Some(declaration) = &export.declaration {
          self.register_exported_declaration(declaration)?;
        }
      }
      _ => {}
    }
    Ok(())
  }

  pub(super) fn register_source_import(
    &mut self,
    import: &oast::ImportDeclaration<'_>,
    fallback: Type,
  ) {
    let Some(specifiers) = &import.specifiers else {
      return;
    };
    for specifier in specifiers {
      match specifier {
        oast::ImportDeclarationSpecifier::ImportNamespaceSpecifier(namespace) => {
          self
            .scope_mut()
            .insert(namespace.local.name.to_string(), fallback.clone());
        }
        oast::ImportDeclarationSpecifier::ImportDefaultSpecifier(default) => {
          self
            .scope_mut()
            .insert(default.local.name.to_string(), fallback.clone());
        }
        oast::ImportDeclarationSpecifier::ImportSpecifier(named) => {
          self
            .scope_mut()
            .insert(named.local.name.to_string(), fallback.clone());
          self.named_types.insert(
            named.local.name.to_string(),
            TypeDefinition {
              parameters: Vec::new(),
              body: fallback.clone(),
            },
          );
        }
      }
    }
  }

  pub(super) fn register_import(
    &mut self,
    import: &oast::ImportDeclaration<'_>,
  ) -> Result<(), TypeCheckerError> {
    let source = import.source.value.as_str();
    let Some(module) = self.modules.get(source).cloned() else {
      if self.source_imports.contains(source) {
        self.register_source_import(import, Type::Unknown);
        return Ok(());
      }
      return Err(TypeCheckerError::from(format!(
        "Unknown declaration module '{source}'"
      )));
    };
    let Some(specifiers) = &import.specifiers else {
      return Ok(());
    };
    for specifier in specifiers {
      match specifier {
        oast::ImportDeclarationSpecifier::ImportNamespaceSpecifier(namespace) => {
          self.scope_mut().insert(
            namespace.local.name.to_string(),
            Type::Object(module_namespace_values(&module)),
          );
          register_namespace_types(
            namespace.local.name.as_str(),
            &module,
            &mut self.namespace_types,
          );
        }
        oast::ImportDeclarationSpecifier::ImportDefaultSpecifier(default) => {
          let typ = module.values.get("default").ok_or_else(|| {
            TypeCheckerError::from(format!("Module '{source}' has no default export"))
          })?;
          self
            .scope_mut()
            .insert(default.local.name.to_string(), typ.clone());
        }
        oast::ImportDeclarationSpecifier::ImportSpecifier(named) => {
          let imported = module_export_name(&named.imported)?;
          if let Some(typ) = module.values.get(&imported) {
            self
              .scope_mut()
              .insert(named.local.name.to_string(), typ.clone());
          } else if let Some(definition) = module.types.get(&imported) {
            self
              .named_types
              .insert(named.local.name.to_string(), definition.clone());
          } else {
            return Err(TypeCheckerError::from(format!(
              "Module '{source}' has no export '{imported}'"
            )));
          }
        }
      }
    }
    Ok(())
  }

  pub(super) fn register_exported_declaration(
    &mut self,
    declaration: &oast::Declaration<'_>,
  ) -> Result<(), TypeCheckerError> {
    match declaration {
      oast::Declaration::TSTypeAliasDeclaration(alias) => {
        self.named_types.insert(
          alias.id.name.to_string(),
          type_definition(alias.type_parameters.as_deref(), &alias.type_annotation)?,
        );
      }
      oast::Declaration::TSInterfaceDeclaration(interface) => {
        let typ = self.interface_type(interface)?;
        self.named_types.insert(
          interface.id.name.to_string(),
          TypeDefinition {
            parameters: convert_type_parameters(interface.type_parameters.as_deref())?,
            body: typ,
          },
        );
      }
      oast::Declaration::FunctionDeclaration(function) => {
        if let Some(id) = &function.id {
          let typ = self.function_type(&function.params, function.return_type.as_deref())?;
          merge_callable_scope(self.scope_mut(), id.name.as_str(), typ);
        }
      }
      oast::Declaration::VariableDeclaration(variable) => {
        for declarator in &variable.declarations {
          let oast::BindingPattern::BindingIdentifier(identifier) = &declarator.id else {
            return Err(TypeCheckerError::from(
              "unsupported variable binding pattern",
            ));
          };
          let typ = self.annotated_type(declarator.type_annotation.as_deref(), Type::Unknown)?;
          self
            .scope_mut()
            .insert(identifier.name.to_string(), typ);
        }
      }
      _ => {}
    }
    Ok(())
  }

  pub(super) fn interface_type(
    &mut self,
    interface: &oast::TSInterfaceDeclaration<'_>,
  ) -> Result<Type, TypeCheckerError> {
    let mut properties = HashMap::new();
    let mut call_signatures = Vec::new();
    for signature in &interface.body.body {
      match signature {
        oast::TSSignature::TSPropertySignature(property) => {
          if let Some(name) = property_name(&property.key) {
            let typ = property
              .type_annotation
              .as_deref()
              .map(convert_type_ann)
              .transpose()?
              .unwrap_or(Type::Unknown);
            properties.insert(
              name,
              if property.optional {
                Type::Union(vec![typ, Type::Undefined])
              } else {
                typ
              },
            );
          }
        }
        oast::TSSignature::TSMethodSignature(method) => {
          if let Some(name) = property_name(&method.key) {
            properties.insert(
              name,
              self.function_type(&method.params, method.return_type.as_deref())?,
            );
          }
        }
        oast::TSSignature::TSCallSignatureDeclaration(call) => {
          let typ = function_type_from_call_signature(call)?;
          self.record_function_parameter_metadata(&typ, &call.params);
          call_signatures.push(typ);
        }
        _ => {}
      }
    }
    if !call_signatures.is_empty() {
      if properties.is_empty() {
        return Ok(normalize_union(call_signatures));
      }
      properties.insert("[[call]]".to_string(), normalize_union(call_signatures));
    }
    Ok(Type::Object(properties))
  }

  pub(super) fn function_type(
    &mut self,
    params: &oast::FormalParameters<'_>,
    return_type: Option<&oast::TSTypeAnnotation<'_>>,
  ) -> Result<Type, TypeCheckerError> {
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
    let return_type = return_type
      .map(convert_type_ann)
      .transpose()?
      .unwrap_or(Type::Any);
    let typ = Type::Function(parameter_types, Box::new(return_type));
    self.record_function_parameter_metadata(&typ, params);
    Ok(typ)
  }

  pub(super) fn record_function_parameter_metadata(
    &mut self,
    typ: &Type,
    params: &oast::FormalParameters<'_>,
  ) {
    let names = params
      .items
      .iter()
      .map(|parameter| binding_parameter_name(&parameter.pattern))
      .chain(
        params
          .rest
          .as_ref()
          .map(|rest| binding_parameter_name(&rest.rest.argument)),
      )
      .collect();
    self.function_parameter_metadata.insert(
      type_to_string(typ),
      FunctionParameterMetadata {
        names,
        rest_index: params.rest.as_ref().map(|_| params.items.len()),
      },
    );
  }

  pub(super) fn function_parameter_metadata(
    &self,
    typ: &Type,
  ) -> Option<&FunctionParameterMetadata> {
    self
      .function_parameter_metadata
      .get(&type_to_string(typ))
  }

  pub(super) fn annotated_type(
    &mut self,
    annotation: Option<&oast::TSTypeAnnotation<'_>>,
    fallback: Type,
  ) -> Result<Type, TypeCheckerError> {
    let Some(annotation) = annotation else {
      return Ok(fallback);
    };
    let typ = convert_type_ann(annotation)?;
    self.record_type_annotation_metadata(&annotation.type_annotation, &typ);
    Ok(typ)
  }

  pub(super) fn record_type_annotation_metadata(
    &mut self,
    annotation: &oast::TSType<'_>,
    typ: &Type,
  ) {
    match (annotation, typ) {
      (oast::TSType::TSFunctionType(function), Type::Function(..)) => {
        self.record_function_parameter_metadata(typ, &function.params);
      }
      (oast::TSType::TSParenthesizedType(parenthesized), typ) => {
        self.record_type_annotation_metadata(&parenthesized.type_annotation, typ);
      }
      (oast::TSType::TSUnionType(union), Type::Union(types)) => {
        for (annotation, typ) in union.types.iter().zip(types) {
          self.record_type_annotation_metadata(annotation, typ);
        }
      }
      (oast::TSType::TSIntersectionType(intersection), Type::Intersection(types)) => {
        for (annotation, typ) in intersection.types.iter().zip(types) {
          self.record_type_annotation_metadata(annotation, typ);
        }
      }
      _ => {}
    }
  }

  pub(super) fn copy_function_parameter_metadata(
    &mut self,
    source: &Type,
    target: &Type,
  ) {
    if let Some(metadata) = self
      .function_parameter_metadata
      .get(&type_to_string(source))
      .cloned()
    {
      self
        .function_parameter_metadata
        .insert(type_to_string(target), metadata);
    }
    match (source, target) {
      (Type::Array(source), Type::Array(target)) | (Type::Rest(source), Type::Rest(target)) => {
        self.copy_function_parameter_metadata(source, target);
      }
      (Type::Tuple(source), Type::Tuple(target))
      | (Type::Union(source), Type::Union(target))
      | (Type::Intersection(source), Type::Intersection(target)) => {
        for (source, target) in source.iter().zip(target) {
          self.copy_function_parameter_metadata(source, target);
        }
      }
      (Type::Object(source), Type::Object(target)) => {
        for (name, source) in source {
          if let Some(target) = target.get(name) {
            self.copy_function_parameter_metadata(source, target);
          }
        }
      }
      (Type::Function(source, source_return), Type::Function(target, target_return)) => {
        for (source, target) in source.iter().zip(target) {
          self.copy_function_parameter_metadata(source, target);
        }
        self.copy_function_parameter_metadata(source_return, target_return);
      }
      _ => {}
    }
  }
}
