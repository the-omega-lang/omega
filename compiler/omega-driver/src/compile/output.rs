use super::*;

impl Driver {
    pub(super) fn report_unused_imports(&mut self, path: &[Ident], warnings: &mut TaggedWarnings) {
        for (alias, import) in &self.modules.index(path).imports {
            if self.imports.was_used(path, alias) {
                continue;
            }
            let kind = AnalysisWarningKind::UnusedImport {
                alias: alias.clone(),
            };
            if import.suppress.iter().any(|s| s.as_ref() == kind.name()) {
                continue;
            }
            warnings.push((
                path.to_vec(),
                AnalysisWarning::new(import.id, import.span, kind),
            ));
        }
    }

    pub(super) fn collect_extern_functions(&self) -> Vec<ExternFunctionRef> {
        let mut functions = Vec::new();

        for (key, item) in self.items.resolved_items() {
            if key.is_instantiation() || !self.roots.is_extern(&key.module) {
                continue;
            }
            let ResolvedItem::Value {
                r#type: ResolvedType::Function(fn_type),
                storage: Storage::Function,
                decl_id,
                mutable: _,
            } = item
            else {
                continue;
            };
            functions.push(ExternFunctionRef {
                decl_id: *decl_id,
                module_path: key.module.clone(),
                kind: ExternFunctionKind::Free(key.name.clone()),
                fn_type: fn_type.clone(),
                mangling: self.mangling_of(decl_id),
            });
        }

        // Free-function *overloads* live in their own cache, addressed by
        // position rather than by name -- the function's own name/id are read
        // back off the parsed HIR at that same position.
        for ((module_path, index), fn_type) in &self.items.overload_signatures {
            if !self.roots.is_extern(module_path) {
                continue;
            }
            let HirItem::FunctionDefinition(f) =
                &self.modules.parsed(module_path).hir.items[*index]
            else {
                unreachable!("only a function is ever recorded as an overload candidate");
            };
            functions.push(ExternFunctionRef {
                decl_id: f.id,
                module_path: module_path.clone(),
                kind: ExternFunctionKind::Free(f.name.clone()),
                fn_type: fn_type.clone(),
                mangling: self.mangling_of(&f.id),
            });
        }

        for (key, methods) in self.items.cells.all_methods() {
            if key.is_instantiation() || !self.roots.is_extern(&key.module) {
                continue;
            }
            for (method_name, method) in methods {
                functions.push(ExternFunctionRef {
                    decl_id: method.decl_id,
                    module_path: key.module.clone(),
                    kind: ExternFunctionKind::Method {
                        type_name: key.name.clone(),
                        method_name,
                    },
                    mangling: method.annotations.mangling,
                    fn_type: method.fn_type,
                });
            }
        }

        for (key, gap) in &self.items.gaps {
            if !self.roots.is_extern(&key.module) {
                continue;
            }
            for (fn_name, gap_fn) in &gap.functions {
                functions.push(ExternFunctionRef {
                    decl_id: gap_fn.decl_id,
                    module_path: gap.module_path.clone(),
                    kind: ExternFunctionKind::Free(fn_name.clone()),
                    fn_type: gap_fn.fn_type.clone(),
                    mangling: ManglingMode::Glued {
                        spec_module_path: gap.module_path.clone(),
                        spec_name: gap.name.clone(),
                        function_name: fn_name.clone(),
                    },
                });
            }
        }

        for entry in &self.primitives.entries {
            if entry.monomorphized || !self.roots.is_extern(&entry.module) {
                continue;
            }
            for (method_name, method) in &entry.methods {
                functions.push(ExternFunctionRef {
                    decl_id: method.decl_id,
                    module_path: entry.module.clone(),
                    kind: ExternFunctionKind::Primitive {
                        target: entry.target.clone(),
                        method_name: method_name.clone(),
                    },
                    fn_type: method.fn_type.clone(),
                    mangling: method.annotations.mangling.clone(),
                });
            }
        }

        for entry in &self.conformances.entries {
            if entry.origin != ConformanceOrigin::Concrete || !self.roots.is_extern(&entry.module) {
                continue;
            }
            for (method_name, method) in &entry.methods {
                if method.source.is_none() {
                    continue;
                }
                functions.push(ExternFunctionRef {
                    decl_id: method.decl_id,
                    module_path: entry.module.clone(),
                    kind: ExternFunctionKind::Conform {
                        target: entry.target.clone(),
                        spec_name: entry.spec.borrow().name.clone(),
                        spec_args: entry.spec_args.clone(),
                        method_name: method_name.clone(),
                    },
                    fn_type: method.fn_type.clone(),
                    mangling: method.annotations.mangling.clone(),
                });
            }
        }

        functions
    }

    fn mangling_of(&self, decl_id: &HirId) -> ManglingMode {
        self.items
            .function_annotations
            .get(decl_id)
            .map(|a| a.mangling.clone())
            .unwrap_or_default()
    }

    pub(super) fn sweep_dead_code(&self, local: &[ModulePath], usage: &FieldUsage) -> TaggedWarnings {
        let mut warnings = TaggedWarnings::new();

        let unused_field = |owner: &Ident, field: &HirField| {
            AnalysisWarning::new(
                field.id,
                field.span,
                AnalysisWarningKind::UnusedField {
                    owner: owner.clone(),
                    field: field.ident.clone(),
                },
            )
        };

        for decl in group_by_declaration(self.items.cells.structs(), |c| (c.id, c.suppress.clone()))
        {
            let Some(def) = self.hir_struct(decl.module, decl.name) else {
                continue;
            };
            if !local.contains(decl.module) || decl.suppresses("unused_field") {
                continue;
            }
            for (index, field) in def.fields.iter().enumerate() {
                if !decl.any(|id| usage.struct_fields.contains(&(id, index))) {
                    warnings.push((decl.module.clone(), unused_field(decl.name, field)));
                }
            }
        }

        for decl in group_by_declaration(self.items.cells.unions(), |c| (c.id, c.suppress.clone()))
        {
            let Some(def) = self.hir_union(decl.module, decl.name) else {
                continue;
            };
            if !local.contains(decl.module) || decl.suppresses("unused_field") {
                continue;
            }
            for (index, field) in def.fields.iter().enumerate() {
                if !decl.any(|id| usage.union_fields.contains(&(id, index))) {
                    warnings.push((decl.module.clone(), unused_field(decl.name, field)));
                }
            }
        }

        for decl in group_by_declaration(self.items.cells.enums(), |c| (c.id, c.suppress.clone())) {
            let Some(def) = self.hir_enum(decl.module, decl.name) else {
                continue;
            };
            if !local.contains(decl.module) {
                continue;
            }

            if !decl.suppresses("unused_field") {
                for (index, field) in def.dynamic_fields.iter().enumerate() {
                    if !decl.any(|id| usage.enum_dynamic_fields.contains(&(id, index))) {
                        warnings.push((decl.module.clone(), unused_field(decl.name, field)));
                    }
                }
                for (variant_index, variant) in def.variants.iter().enumerate() {
                    for (field_index, field) in variant.fields.iter().enumerate() {
                        if !decl.any(|id| {
                            usage
                                .enum_body_fields
                                .contains(&(id, variant_index, field_index))
                        }) {
                            warnings.push((decl.module.clone(), unused_field(decl.name, field)));
                        }
                    }
                }
            }

            if !decl.suppresses("never_constructed_variant") {
                for (variant_index, variant) in def.variants.iter().enumerate() {
                    if decl.any(|id| usage.enum_variants.contains(&(id, variant_index))) {
                        continue;
                    }
                    warnings.push((
                        decl.module.clone(),
                        AnalysisWarning::new(
                            variant.id,
                            variant.span,
                            AnalysisWarningKind::NeverConstructedVariant {
                                r#enum: decl.name.clone(),
                                variant: variant.name.clone(),
                            },
                        ),
                    ));
                }
            }
        }

        // Each of the three loops above is already deterministic on its own
        // (the cell caches preserve creation order). This sort is for
        // something they can't give separately: one chronological ordering
        // across all three kinds together, instead of every struct warning,
        // then every union warning, then every enum warning.
        warnings.sort_by(|(a_path, a), (b_path, b)| {
            let key = |path: &ModulePath| {
                path.iter()
                    .map(|i| i.as_ref().to_string())
                    .collect::<Vec<_>>()
            };
            key(a_path)
                .cmp(&key(b_path))
                .then(a.span.start.cmp(&b.span.start))
        });
        warnings
    }

    fn hir_struct(&self, module: &[Ident], name: &Ident) -> Option<&HirStructDef> {
        match self.modules.item(module, name)? {
            HirItem::Struct(s) => Some(s),
            _ => None,
        }
    }

    fn hir_union(&self, module: &[Ident], name: &Ident) -> Option<&HirUnionDef> {
        match self.modules.item(module, name)? {
            HirItem::Union(u) => Some(u),
            _ => None,
        }
    }

    fn hir_enum(&self, module: &[Ident], name: &Ident) -> Option<&HirEnumDef> {
        match self.modules.item(module, name)? {
            HirItem::Enum(e) => Some(e),
            _ => None,
        }
    }
}

struct Declaration<'a> {
    module: &'a ModulePath,
    name: &'a Ident,
    ids: Vec<HirId>,
    suppress: Vec<Ident>,
}

impl Declaration<'_> {
    fn suppresses(&self, warning: &str) -> bool {
        self.suppress.iter().any(|s| s.as_ref() == warning)
    }

    fn any(&self, used: impl Fn(HirId) -> bool) -> bool {
        self.ids.iter().copied().any(used)
    }
}

fn group_by_declaration<'a, T>(
    cells: impl Iterator<Item = (&'a ItemKey, &'a Rc<RefCell<T>>)>,
    facts: impl Fn(&T) -> (HirId, Vec<Ident>),
) -> Vec<Declaration<'a>>
where
    T: 'a,
{
    let mut grouped: IndexMap<(&ModulePath, &Ident), Declaration<'a>> = IndexMap::new();
    for (key, cell) in cells {
        let (id, suppress) = facts(&cell.borrow());
        grouped
            .entry((&key.module, &key.name))
            .or_insert_with(|| Declaration {
                module: &key.module,
                name: &key.name,
                ids: vec![],
                suppress,
            })
            .ids
            .push(id);
    }
    grouped.into_values().collect()
}
