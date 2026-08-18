use super::*;

pub(super) struct Expander<'a> {
    defs: &'a HashMap<Ident, MacroDefinitionStmt>,
    budget: u32,
    state: &'a mut ExpansionState,
}

impl<'a> Expander<'a> {
    pub(super) fn new(
        defs: &'a HashMap<Ident, MacroDefinitionStmt>,
        state: &'a mut ExpansionState,
    ) -> Self {
        Self {
            defs,
            budget: MAX_EXPANSIONS,
            state,
        }
    }

    fn macro_definition(
        &self,
        invocation: &MacroInvocationExpr,
    ) -> Result<MacroDefinitionStmt, MacroError> {
        self.state
            .definition_for(invocation.origin, self.defs, &invocation.name)
            .ok_or_else(|| MacroError::UnknownMacro {
                name: invocation.name.clone(),
            })
    }

    pub(super) fn expand_item_list(
        &mut self,
        nodes: Vec<ItemNode>,
    ) -> Result<Vec<ItemNode>, MacroError> {
        let mut result = Vec::with_capacity(nodes.len());
        for node in nodes {
            match node.item {
                Item::MacroInvocation(inv) => {
                    result.extend(self.expand_items_invocation(&inv, node.span)?);
                }
                Item::FunctionDefinition(f) => result.push(ItemNode {
                    item: Item::FunctionDefinition(self.expand_function_def(f)?),
                    span: node.span,
                }),
                Item::Struct(s) => result.push(ItemNode {
                    item: Item::Struct(self.expand_struct_def(s)?),
                    span: node.span,
                }),
                Item::Enum(e) => result.push(ItemNode {
                    item: Item::Enum(self.expand_enum_def(e)?),
                    span: node.span,
                }),
                Item::Union(u) => result.push(ItemNode {
                    item: Item::Union(self.expand_union_def(u)?),
                    span: node.span,
                }),
                Item::Spec(sp) => result.push(ItemNode {
                    item: Item::Spec(self.expand_spec_def(sp)?),
                    span: node.span,
                }),
                Item::Gap(gap) => result.push(ItemNode {
                    item: Item::Gap(gap),
                    span: node.span,
                }),
                Item::Glue(mut glue) => {
                    glue.functions = glue
                        .functions
                        .into_iter()
                        .map(|f| self.expand_function_def(f))
                        .collect::<Result<_, _>>()?;
                    result.push(ItemNode {
                        item: Item::Glue(glue),
                        span: node.span,
                    });
                }
                Item::Conform(mut conform) => {
                    conform.functions = conform
                        .functions
                        .into_iter()
                        .map(|f| self.expand_function_def(f))
                        .collect::<Result<_, _>>()?;
                    result.push(ItemNode {
                        item: Item::Conform(conform),
                        span: node.span,
                    });
                }
                Item::Primitive(mut primitive) => {
                    primitive.functions = primitive
                        .functions
                        .into_iter()
                        .map(|f| self.expand_function_def(f))
                        .collect::<Result<_, _>>()?;
                    result.push(ItemNode {
                        item: Item::Primitive(primitive),
                        span: node.span,
                    });
                }
                other @ (Item::Declaration(_) | Item::ExternDeclaration(_) | Item::Import(_)) => {
                    result.push(ItemNode {
                        item: other,
                        span: node.span,
                    });
                }
                Item::Walrus(w) => result.push(ItemNode {
                    item: Item::Walrus(WalrusStmt {
                        value: self.expand_expr(w.value)?,
                        ..w
                    }),
                    span: node.span,
                }),
                Item::DeclarationWithInit(decl, value) => result.push(ItemNode {
                    item: Item::DeclarationWithInit(decl, self.expand_expr(value)?),
                    span: node.span,
                }),
                Item::MacroDefinition(def) => {
                    return Err(MacroError::MacroDefinitionInExpansion {
                        macro_name: def.name,
                    });
                }
            }
        }
        Ok(result)
    }

    fn expand_items_invocation(
        &mut self,
        inv: &MacroInvocationExpr,
        call_span: Span,
    ) -> Result<Vec<ItemNode>, MacroError> {
        let def = self.macro_definition(inv)?;
        let tokens = self.substitute_invocation(&def, &inv.args, call_span)?;
        let padded = with_eof(&tokens);
        let mut p = Parser::new(&padded);
        let nodes = crate::parser::item::parse_source_module(&mut p);
        let errors = p.into_errors();
        if !errors.is_empty() {
            return Err(MacroError::ExpansionParseError {
                macro_name: inv.name.clone(),
                position: MacroPosition::Item,
                errors: join_errors(&errors),
            });
        }
        self.expand_item_list(nodes)
    }

    fn expand_expr_invocation(
        &mut self,
        inv: &MacroInvocationExpr,
        call_span: Span,
    ) -> Result<ExpressionNode, MacroError> {
        let def = self.macro_definition(inv)?;
        let tokens = self.substitute_invocation(&def, &inv.args, call_span)?;
        let padded = with_eof(&tokens);
        let mut p = Parser::new(&padded);
        let parsed = crate::parser::expression::parse_expression(&mut p);
        let fully_consumed = p.is_eof();
        let errors = p.into_errors();
        let node = match parsed {
            Some(node) if fully_consumed && errors.is_empty() => node,
            _ => {
                let message = if errors.is_empty() {
                    "unexpected trailing tokens".to_string()
                } else {
                    join_errors(&errors)
                };
                return Err(MacroError::ExpansionParseError {
                    macro_name: inv.name.clone(),
                    position: MacroPosition::Expression,
                    errors: message,
                });
            }
        };
        self.expand_expr(node)
    }

    fn expand_statements_invocation(
        &mut self,
        inv: &MacroInvocationExpr,
        call_span: Span,
    ) -> Result<Vec<StatementNode>, MacroError> {
        let def = self.macro_definition(inv)?;
        let tokens = self.substitute_invocation(&def, &inv.args, call_span)?;
        let padded = with_eof(&tokens);
        let mut p = Parser::new(&padded);
        let parsed = p.allow_struct_literals(crate::parser::expression::parse_block_contents);
        let fully_consumed = p.is_eof();
        let errors = p.into_errors();
        let mut cb = match parsed {
            Some(cb) if fully_consumed && errors.is_empty() => cb,
            _ => {
                let message = if errors.is_empty() {
                    "unexpected trailing tokens".to_string()
                } else {
                    join_errors(&errors)
                };
                return Err(MacroError::ExpansionParseError {
                    macro_name: inv.name.clone(),
                    position: MacroPosition::Statement,
                    errors: message,
                });
            }
        };
        if let Some(tail) = cb.tail.take() {
            cb.statements.push(StatementNode {
                span: tail.span,
                statement: Statement::Expression(*tail),
            });
        }
        self.expand_statement_list(cb.statements)
    }

    fn substitute_invocation(
        &mut self,
        def: &MacroDefinitionStmt,
        args: &[Vec<Token>],
        call_span: Span,
    ) -> Result<Vec<Token>, MacroError> {
        let fixed_len = def.signature.fixed.len();
        let expected = if def.signature.variadic.is_some() {
            Arity::AtLeast(fixed_len)
        } else {
            Arity::Exact(fixed_len)
        };
        if (def.signature.variadic.is_some() && args.len() < fixed_len)
            || (def.signature.variadic.is_none() && args.len() != fixed_len)
        {
            return Err(MacroError::ArgCountMismatch {
                macro_name: def.name.clone(),
                expected,
                found: args.len(),
            });
        }
        if self.budget == 0 {
            return Err(MacroError::ExpansionLimitExceeded {
                macro_name: def.name.clone(),
            });
        }
        self.budget -= 1;

        let mut bindings = Bindings::default();
        for (param, arg) in def.signature.fixed.iter().zip(args.iter()) {
            validate_fragment(def, param, arg)?;
            bindings.0.insert(param.name.clone(), Binding::One(arg));
        }
        if let Some(param) = &def.signature.variadic {
            for arg in &args[fixed_len..] {
                validate_fragment(def, param, arg)?;
            }
            bindings
                .0
                .insert(param.name.clone(), Binding::Many(&args[fixed_len..]));
        }
        let mut out = Vec::new();
        let origin = self.state.fresh_origin(def);
        render(
            &def.body,
            &bindings,
            def.signature.variadic.as_ref().map(|p| &p.name),
            origin,
            &mut out,
        );
        // Generated tokens use call-site spans because `Span` has no file identity.
        Ok(out
            .into_iter()
            .map(|token| Token {
                kind: token.kind,
                span: call_span,
                origin: token.origin,
            })
            .collect())
    }

    fn expand_function_def(
        &mut self,
        f: FunctionDefinitionStmt,
    ) -> Result<FunctionDefinitionStmt, MacroError> {
        Ok(FunctionDefinitionStmt {
            codeblock: self.expand_codeblock(f.codeblock)?,
            ..f
        })
    }

    fn expand_member_functions(
        &mut self,
        functions: Vec<FunctionDefinitionStmt>,
    ) -> Result<Vec<FunctionDefinitionStmt>, MacroError> {
        functions
            .into_iter()
            .map(|f| self.expand_function_def(f))
            .collect()
    }

    fn expand_struct_def(&mut self, s: StructStmt) -> Result<StructStmt, MacroError> {
        let functions = self.expand_member_functions(s.functions)?;
        Ok(StructStmt { functions, ..s })
    }

    fn expand_union_def(&mut self, u: UnionStmt) -> Result<UnionStmt, MacroError> {
        let functions = self.expand_member_functions(u.functions)?;
        Ok(UnionStmt { functions, ..u })
    }

    fn expand_enum_def(&mut self, e: EnumStmt) -> Result<EnumStmt, MacroError> {
        let variants = e
            .variants
            .into_iter()
            .map(|v| {
                let args = v
                    .args
                    .into_iter()
                    .map(|a| self.expand_expr(a))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(EnumVariantStmt { args, ..v })
            })
            .collect::<Result<Vec<_>, MacroError>>()?;
        let functions = self.expand_member_functions(e.functions)?;
        Ok(EnumStmt {
            variants,
            functions,
            ..e
        })
    }

    fn expand_spec_def(&mut self, sp: SpecStmt) -> Result<SpecStmt, MacroError> {
        let functions = sp
            .functions
            .into_iter()
            .map(|f| {
                let body = f.body.map(|b| self.expand_codeblock(b)).transpose()?;
                Ok(SpecFunctionStmt { body, ..f })
            })
            .collect::<Result<Vec<_>, MacroError>>()?;
        Ok(SpecStmt { functions, ..sp })
    }

    fn expand_codeblock(&mut self, cb: CodeblockExpr) -> Result<CodeblockExpr, MacroError> {
        let statements = self.expand_statement_list(cb.statements)?;
        let tail = cb
            .tail
            .map(|t| self.expand_expr(*t).map(Box::new))
            .transpose()?;
        Ok(CodeblockExpr {
            statements,
            tail,
            span: cb.span,
        })
    }

    fn expand_statement_list(
        &mut self,
        statements: Vec<StatementNode>,
    ) -> Result<Vec<StatementNode>, MacroError> {
        let mut result = Vec::with_capacity(statements.len());
        for node in statements {
            match node.statement {
                Statement::MacroInvocation(inv) => {
                    result.extend(self.expand_statements_invocation(&inv, node.span)?)
                }
                statement => result.push(self.expand_stmt_node(StatementNode {
                    statement,
                    span: node.span,
                })?),
            }
        }
        Ok(result)
    }

    fn expand_if(&mut self, if_expr: IfExpr) -> Result<IfExpr, MacroError> {
        let branches = if_expr
            .branches
            .into_iter()
            .map(|(cond, block)| Ok((self.expand_expr(cond)?, self.expand_codeblock(block)?)))
            .collect::<Result<Vec<_>, MacroError>>()?;
        let else_branch = if_expr
            .else_branch
            .map(|b| self.expand_codeblock(b))
            .transpose()?;
        Ok(IfExpr {
            branches,
            else_branch,
        })
    }

    fn expand_stmt_node(&mut self, node: StatementNode) -> Result<StatementNode, MacroError> {
        let span = node.span;
        let statement = self.expand_statement(node.statement)?;
        Ok(StatementNode { statement, span })
    }

    fn expand_statement(&mut self, statement: Statement) -> Result<Statement, MacroError> {
        Ok(match statement {
            Statement::MacroInvocation(_) => {
                unreachable!("statement invocations are spliced by expand_statement_list")
            }
            Statement::Declaration(decl) => Statement::Declaration(decl),
            Statement::DeclarationWithInit(decl, value) => {
                Statement::DeclarationWithInit(decl, self.expand_expr(value)?)
            }
            Statement::ExternDeclaration(decl) => Statement::ExternDeclaration(decl),
            Statement::Expression(expr) => Statement::Expression(self.expand_expr(expr)?),
            Statement::Return(ret) => Statement::Return(ReturnStmt {
                return_value: self.expand_expr(ret.return_value)?,
            }),
            Statement::Break => Statement::Break,
            Statement::Continue => Statement::Continue,
            Statement::Walrus(w) => Statement::Walrus(WalrusStmt {
                value: self.expand_expr(w.value)?,
                ..w
            }),
            Statement::While(w) => Statement::While(WhileStmt {
                condition: self.expand_expr(w.condition)?,
                body: self.expand_codeblock(w.body)?,
            }),
            Statement::Loop(l) => Statement::Loop(LoopStmt {
                body: self.expand_codeblock(l.body)?,
            }),
            Statement::For(f) => {
                let f = *f;
                Statement::For(Box::new(ForStmt {
                    init: f.init.map(|s| self.expand_statement(s)).transpose()?,
                    condition: f.condition.map(|c| self.expand_expr(c)).transpose()?,
                    post: f.post.map(|p| self.expand_expr(p)).transpose()?,
                    body: self.expand_codeblock(f.body)?,
                }))
            }
            Statement::ForIn(f) => {
                let f = *f;
                Statement::ForIn(Box::new(ForInStmt {
                    iterator: self.expand_expr(f.iterator)?,
                    body: self.expand_codeblock(f.body)?,
                    ..f
                }))
            }
            Statement::Defer(d) => Statement::Defer(DeferStmt {
                body: Box::new(self.expand_statement(*d.body)?),
            }),
        })
    }

    fn expand_expr(&mut self, node: ExpressionNode) -> Result<ExpressionNode, MacroError> {
        let span = node.span;
        if let Expression::MacroInvocation(inv) = node.expression {
            let expanded = self.expand_expr_invocation(&inv, span)?;
            return Ok(ExpressionNode {
                expression: expanded.expression,
                span,
            });
        }

        let expression = match node.expression {
            Expression::MacroInvocation(_) => unreachable!("handled above"),
            Expression::Path(p) => Expression::Path(p),
            Expression::FieldAccess(access) => Expression::FieldAccess(Box::new(FieldAccessExpr {
                base: self.expand_expr(access.base)?,
                field: access.field,
            })),
            Expression::Index(index) => Expression::Index(Box::new(IndexExpr {
                base: self.expand_expr(index.base)?,
                index: self.expand_expr(index.index)?,
            })),
            Expression::Deref(deref) => Expression::Deref(Box::new(DerefExpr {
                base: self.expand_expr(deref.base)?,
            })),
            Expression::AddressOf(addr) => Expression::AddressOf(Box::new(AddressOfExpr {
                base: self.expand_expr(addr.base)?,
                mutable: addr.mutable,
            })),
            Expression::Negate(neg) => Expression::Negate(Box::new(NegateExpr {
                base: self.expand_expr(neg.base)?,
            })),
            Expression::BitNot(not) => Expression::BitNot(Box::new(BitNotExpr {
                base: self.expand_expr(not.base)?,
            })),
            Expression::Not(not) => Expression::Not(Box::new(NotExpr {
                base: self.expand_expr(not.base)?,
            })),
            Expression::Logical(logical) => Expression::Logical(Box::new(LogicalExpr {
                op: logical.op,
                left: self.expand_expr(logical.left)?,
                right: self.expand_expr(logical.right)?,
            })),
            Expression::Reveal(reveal) => Expression::Reveal(Box::new(RevealExpr {
                base: self.expand_expr(reveal.base)?,
            })),
            Expression::Comp(comp) => Expression::Comp(Box::new(CompExpr {
                base: self.expand_expr(comp.base)?,
            })),
            Expression::Cast(cast) => Expression::Cast(Box::new(CastExpr {
                target: cast.target,
                base: self.expand_expr(cast.base)?,
            })),
            // Bare types cannot contain expression-position macro metavariables.
            Expression::Sizeof(sizeof) => Expression::Sizeof(sizeof),
            Expression::Increment(incr) => Expression::Increment(Box::new(IncrementExpr {
                base: self.expand_expr(incr.base)?,
            })),
            Expression::Decrement(decr) => Expression::Decrement(Box::new(DecrementExpr {
                base: self.expand_expr(decr.base)?,
            })),
            Expression::BinaryOp(bin) => Expression::BinaryOp(Box::new(BinaryOpExpr {
                left: self.expand_expr(bin.left)?,
                op: bin.op,
                right: self.expand_expr(bin.right)?,
            })),
            Expression::Number(n) => Expression::Number(n),
            Expression::String(s) => Expression::String(s),
            Expression::ByteString(s) => Expression::ByteString(s),
            Expression::Bool(b) => Expression::Bool(b),
            Expression::Char(c) => Expression::Char(c),
            Expression::Codeblock(cb) => Expression::Codeblock(self.expand_codeblock(cb)?),
            Expression::If(if_expr) => Expression::If(Box::new(self.expand_if(*if_expr)?)),
            Expression::FunctionCall(call) => Expression::FunctionCall(FunctionCallExpr {
                callee: Box::new(self.expand_expr(*call.callee)?),
                args: call
                    .args
                    .into_iter()
                    .map(|a| self.expand_expr(a))
                    .collect::<Result<Vec<_>, _>>()?,
            }),
            Expression::Assignment(assign) => Expression::Assignment(Box::new(AssignmentExpr {
                target: self.expand_expr(assign.target)?,
                value: Box::new(self.expand_expr(*assign.value)?),
            })),
            Expression::CompoundAssign(assign) => {
                Expression::CompoundAssign(Box::new(CompoundAssignExpr {
                    target: self.expand_expr(assign.target)?,
                    op: assign.op,
                    value: Box::new(self.expand_expr(*assign.value)?),
                }))
            }
            Expression::ArrayLiteral(lit) => Expression::ArrayLiteral(ArrayLiteralExpr {
                elements: lit
                    .elements
                    .into_iter()
                    .map(|e| self.expand_expr(e))
                    .collect::<Result<Vec<_>, _>>()?,
            }),
            Expression::StructLiteral(lit) => Expression::StructLiteral(StructLiteralExpr {
                path: lit.path,
                fields: lit
                    .fields
                    .into_iter()
                    .map(|f| {
                        Ok(StructLiteralField {
                            name: f.name,
                            name_span: f.name_span,
                            value: self.expand_expr(f.value)?,
                        })
                    })
                    .collect::<Result<Vec<_>, MacroError>>()?,
            }),
            Expression::Slice(s) => Expression::Slice(Box::new(SliceExpr {
                base: self.expand_expr(s.base)?,
                range: self.expand_range(s.range)?,
            })),
            Expression::Match(m) => Expression::Match(Box::new(self.expand_match(*m)?)),
            Expression::Range(r) => Expression::Range(Box::new(self.expand_range(*r)?)),
        };
        Ok(ExpressionNode { expression, span })
    }

    fn expand_range(&mut self, range: RangeExpr) -> Result<RangeExpr, MacroError> {
        let end = match range.end {
            RangeEnd::Inclusive(e) => RangeEnd::Inclusive(self.expand_expr(e)?),
            RangeEnd::Exclusive(e) => RangeEnd::Exclusive(self.expand_expr(e)?),
            RangeEnd::Open => RangeEnd::Open,
        };
        Ok(RangeExpr {
            start: range.start.map(|e| self.expand_expr(e)).transpose()?,
            end,
            span: range.span,
        })
    }

    fn expand_match(&mut self, match_expr: MatchExpr) -> Result<MatchExpr, MacroError> {
        let scrutinee = self.expand_expr(match_expr.scrutinee)?;
        let arms = match_expr
            .arms
            .into_iter()
            .map(|arm| {
                let pattern = match arm.pattern {
                    Pattern::Value(v) => Pattern::Value(self.expand_expr(v)?),
                    Pattern::Range(r) => Pattern::Range(self.expand_range(r)?),
                };
                Ok(MatchArm {
                    pattern,
                    body: self.expand_expr(arm.body)?,
                    span: arm.span,
                })
            })
            .collect::<Result<Vec<_>, MacroError>>()?;
        let else_branch = match_expr
            .else_branch
            .map(|b| self.expand_codeblock(b))
            .transpose()?;
        Ok(MatchExpr {
            scrutinee,
            arms,
            else_branch,
            span: match_expr.span,
        })
    }
}
