//! Python extraction: emits the same [`ParsedFile`] shape as the TS/JS visitor
//! so every downstream pass (graph, reachability, findings, edits) works
//! unchanged, while modelling Python's own module and visibility rules.
//!
//! Python differs from ES modules in ways that make sharing the TS/JS visitor
//! unsafe rather than merely awkward:
//!
//! * There is no `export` keyword. A module-level name is importable unless it
//!   is underscore-prefixed; `__all__` declares the explicit `import *` surface.
//! * Packages are directories with `__init__.py`, so a single dotted specifier
//!   can resolve to `pkg/mod.py` or to `pkg/mod/__init__.py`.
//! * `from pkg import thing` is syntactically identical whether `thing` is a
//!   submodule or a plain attribute, so specifiers are recorded as the full
//!   dotted candidate and [`resolve_python_import`] falls back to the parent
//!   module when the longer path does not exist.
//! * Dunder methods and decorated definitions are invoked by the interpreter or
//!   a framework rather than from a visible call site, so they are recorded as
//!   public surface instead of being left to look dead.
//!
//! Receiver typing stays deliberately shallow: Python is duck-typed, so only
//! provable `x = ClassName()` and `x: ClassName` bindings become `var_types`.
//! Nothing here infers a framework: decorated definitions are kept alive
//! regardless of which decorator was applied, so no framework list can drift.

use std::collections::HashSet;
use std::path::{Path, PathBuf};

use tree_sitter::{Node, Parser};

use crate::imports::{ImportKind, ImportRec, ReExportRec, RefContext, RefKind, ReferenceRec};
use crate::parser::{Language, LocalExport, ParsedFile};
use crate::symbols::{Entity, SymbolKind};

/// Span of a declaration: (start_line, end_line, start_byte, end_byte).
type Span = (usize, usize, usize, usize);

pub fn parse_python(abs_path: &Path, rel_path: &str, source: &str) -> Option<ParsedFile> {
    let mut parser = Parser::new();
    parser.set_language(&tree_sitter_python::language()).ok()?;
    let tree = parser.parse(source, None)?;
    let root = tree.root_node();

    let file_name = rel_path.rsplit('/').next().unwrap_or(rel_path);
    let mut visitor = PyVisitor {
        source,
        abs_path: abs_path.to_path_buf(),
        rel_path: rel_path.to_string(),
        is_init: file_name == "__init__.py",
        entities: Vec::new(),
        imports: Vec::new(),
        reexports: Vec::new(),
        local_exports: Vec::new(),
        references: Vec::new(),
        var_types: Vec::new(),
        heritage: Vec::new(),
        spread_vars: Vec::new(),
        dynamic_prefixes: Vec::new(),
        dynamic_details: Vec::new(),
        symbol_stack: Vec::new(),
        class_exported: Vec::new(),
        all_names: Vec::new(),
        publishes_members: false,
        has_top_level_calls: false,
    };

    visitor.collect_all_names(&root);
    visitor.visit_children(&root);
    visitor.finish_public_surface();
    visitor.detect_dynamic_heuristics();

    let has_main_guard = has_main_guard(source);
    Some(ParsedFile {
        abs_path: abs_path.to_path_buf(),
        rel_path: rel_path.to_string(),
        language: Language::Python,
        source: source.to_string(),
        entities: visitor.entities,
        imports: visitor.imports,
        reexports: visitor.reexports,
        local_exports: visitor.local_exports,
        references: visitor.references,
        var_types: visitor.var_types,
        // Object-literal hooks and `module.exports` surfaces are ES-module
        // shapes with no Python counterpart.
        hook_methods: Vec::new(),
        surface_methods: Vec::new(),
        dynamic_dispatch_bases: Vec::new(),
        dynamic_prefixes: visitor.dynamic_prefixes,
        spread_vars: visitor.spread_vars,
        heritage: visitor.heritage,
        // `is_ambient` models `.d.ts` global type declarations; Python has no
        // equivalent (even a bare script's names are module-scoped).
        is_ambient: false,
        escaped_classes: Vec::new(),
        publishes_members: visitor.publishes_members,
        // A `__main__` guard means the file is meant to be run directly, so it
        // must never be reported as an unreferenced module.
        is_executable_script: source.starts_with("#!") || has_main_guard,
        has_top_level_calls: visitor.has_top_level_calls || has_main_guard,
        dynamic_risk: !visitor.dynamic_details.is_empty(),
        dynamic_details: visitor.dynamic_details,
    })
}

struct PyVisitor<'a> {
    source: &'a str,
    abs_path: PathBuf,
    rel_path: String,
    /// `__init__.py` republishes what it imports: names imported there are the
    /// package's public surface, not private helpers.
    is_init: bool,
    entities: Vec<Entity>,
    imports: Vec<ImportRec>,
    reexports: Vec<ReExportRec>,
    local_exports: Vec<LocalExport>,
    references: Vec<ReferenceRec>,
    var_types: Vec<(String, String)>,
    heritage: Vec<(String, String)>,
    spread_vars: Vec<String>,
    dynamic_prefixes: Vec<String>,
    dynamic_details: Vec<String>,
    /// Enclosing entity ids, innermost last.
    symbol_stack: Vec<String>,
    /// `exported` flag of each enclosing class, innermost last: a method is only
    /// reachable from outside when the class holding it is.
    class_exported: Vec<bool>,
    all_names: Vec<String>,
    publishes_members: bool,
    has_top_level_calls: bool,
}

impl<'a> PyVisitor<'a> {
    fn text(&self, node: &Node) -> &'a str {
        node.utf8_text(self.source.as_bytes()).unwrap_or("")
    }

    fn line(&self, node: &Node) -> usize {
        node.start_position().row + 1
    }

    fn current_symbol(&self) -> Option<String> {
        self.symbol_stack.last().cloned()
    }

    fn at_module_level(&self) -> bool {
        self.symbol_stack.is_empty()
    }

    fn in_class_body(&self) -> bool {
        // A class body's direct children run in class scope; a method body does
        // not. `class_exported` only grows for classes, `symbol_stack` for both,
        // so the innermost frame is a class exactly when their depths agree.
        !self.class_exported.is_empty() && self.symbol_stack.len() == self.class_exported.len()
    }

    fn push_ref(
        &mut self,
        node: &Node,
        name: &str,
        base: Option<String>,
        kind: RefKind,
        context: RefContext,
    ) {
        if name.is_empty() {
            return;
        }
        // Member names are never filtered: any identifier can be a method name,
        // and resolution only links when the receiver provably owns it.
        if context != RefContext::Member && is_python_builtin(name) {
            return;
        }
        self.references.push(ReferenceRec {
            from_file: self.rel_path.clone(),
            from_symbol: self.current_symbol(),
            name: name.to_string(),
            base,
            kind,
            context,
            line: self.line(node),
        });
    }

    fn visit_children(&mut self, node: &Node) {
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in &children {
            self.visit(child);
        }
    }

    fn visit(&mut self, node: &Node) {
        match node.kind() {
            "function_definition" => self.visit_function(node, None, false),
            "class_definition" => self.visit_class(node, None, false),
            "decorated_definition" => self.visit_decorated(node),
            "import_statement" => self.visit_import(node),
            "import_from_statement" => self.visit_import_from(node),
            "assignment" => self.visit_assignment(node),
            "augmented_assignment" => {
                // `x += f()`: the left side is read as well as written.
                self.visit_children(node);
            }
            "call" => self.visit_call(node),
            "attribute" => self.visit_attribute(node),
            "decorator" => self.visit_decorator_expr(node),
            "type" => self.visit_type(node),
            "parameters" | "lambda_parameters" => self.visit_parameters(node),
            "keyword_argument" => {
                // The keyword is a parameter label, not a reference to a symbol.
                if let Some(value) = node.child_by_field_name("value") {
                    self.visit(&value);
                }
            }
            "dictionary_splat" | "list_splat" => {
                // `f(**opts)` / `f(*args)`: the spread variable's contents flow
                // into the callee.
                if let Some(inner) = node.named_child(0) {
                    if inner.kind() == "identifier" {
                        let name = self.text(&inner).to_string();
                        if !self.spread_vars.contains(&name) {
                            self.spread_vars.push(name);
                        }
                    }
                }
                self.visit_children(node);
            }
            "identifier" => {
                let name = self.text(node).to_string();
                self.push_ref(node, &name, None, RefKind::Value, RefContext::Identifier);
            }
            "expression_statement" => {
                if self.at_module_level() && contains_call(node) {
                    self.has_top_level_calls = true;
                }
                self.visit_children(node);
            }
            // Comments and string bodies (docstrings) hold no references.
            "comment" | "string" => {}
            _ => self.visit_children(node),
        }
    }

    /// Decorators are part of the declaration: the entity span must cover them
    /// so that removing a symbol removes its decorators too.
    ///
    /// A decorator also *receives the definition*: `@route("/users")` passes
    /// `list_users` to `route`, so the function object escapes to a callable this
    /// analysis cannot follow and may be invoked from anywhere. That is a fact
    /// about the call, not a guess about any framework.
    ///
    /// The exception is the closed set of language- and stdlib-level transformer
    /// decorators (`staticmethod`, `property`, `dataclass`, ...), which rebind
    /// the result to the same name instead of registering it elsewhere. Those
    /// leave the ordinary rules in force, so an unused `@staticmethod` is still
    /// reported.
    fn visit_decorated(&mut self, node: &Node) {
        let span: Span = (
            node.start_position().row + 1,
            node.end_position().row + 1,
            node.start_byte(),
            node.end_byte(),
        );
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();

        let escapes = children
            .iter()
            .filter(|c| c.kind() == "decorator")
            .any(|c| !is_transformer_decorator(&self.decorator_name(c)));

        for child in &children {
            match child.kind() {
                "decorator" => self.visit_decorator_expr(child),
                "function_definition" => self.visit_function(child, Some(span), escapes),
                "class_definition" => self.visit_class(child, Some(span), escapes),
                _ => {}
            }
        }
    }

    /// Dotted name of the decorator being applied, with any call arguments
    /// dropped: `@app.route("/x")` → `app.route`.
    fn decorator_name(&self, decorator: &Node) -> String {
        let mut target = None;
        let mut cursor = decorator.walk();
        for child in decorator.children(&mut cursor) {
            if child.is_named() {
                target = Some(child);
                break;
            }
        }
        let Some(mut node) = target else {
            return String::new();
        };
        if node.kind() == "call" {
            match node.child_by_field_name("function") {
                Some(func) => node = func,
                None => return String::new(),
            }
        }
        self.text(&node).to_string()
    }

    fn visit_decorator_expr(&mut self, node: &Node) {
        // Skip the `@` token; the rest is an ordinary expression whose names are
        // genuine references (`@app.route` uses `app`).
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in &children {
            if child.is_named() {
                self.visit(child);
            }
        }
    }

    fn visit_function(&mut self, node: &Node, span: Option<Span>, escapes: bool) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.text(&name_node).to_string();
        let (start_line, end_line, start_byte, end_byte) = span.unwrap_or((
            node.start_position().row + 1,
            node.end_position().row + 1,
            node.start_byte(),
            node.end_byte(),
        ));

        let in_class = self.in_class_body();

        // Dunder methods implement interpreter protocols: `__init__` runs on
        // construction, `__enter__` on `with`, `__reduce__` on pickling. No call
        // site names them, and removing one changes behaviour, so they are never
        // dead-code candidates and are not recorded as entities. Their bodies are
        // still walked, with references attributed to the owning class so that
        // whatever they use stays reachable.
        if in_class && is_dunder(&name) {
            if let Some(params) = node.child_by_field_name("parameters") {
                self.visit_parameters(&params);
            }
            if let Some(body) = node.child_by_field_name("body") {
                self.visit_children(&body);
            }
            return;
        }

        let kind = if in_class {
            SymbolKind::Method
        } else {
            SymbolKind::Function
        };
        let exported = if in_class {
            // A method is reachable from outside exactly when its class is.
            let class_public = self.class_exported.last().copied().unwrap_or(false);
            class_public && !name.starts_with('_')
        } else {
            escapes || self.is_public_module_name(&name)
        };
        // The decorator call consumes the definition, so the name is genuinely
        // referenced at the point of decoration.
        if escapes {
            self.push_ref(&name_node, &name, None, RefKind::Value, RefContext::Call);
        }

        let entity = Entity::new(
            &self.rel_path,
            self.abs_path.clone(),
            &name,
            kind,
            start_line,
            end_line,
            start_byte,
            end_byte,
            exported,
            self.current_symbol(),
        );
        let id = entity.id.clone();
        self.entities.push(entity);

        // Parameter annotations and defaults are evaluated in the enclosing
        // scope, so they are visited before the body's scope is pushed.
        if let Some(params) = node.child_by_field_name("parameters") {
            self.visit_parameters(&params);
        }
        if let Some(ret) = node.child_by_field_name("return_type") {
            self.visit_type(&ret);
        }

        self.symbol_stack.push(id);
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_children(&body);
        }
        self.symbol_stack.pop();
    }

    fn visit_class(&mut self, node: &Node, span: Option<Span>, escapes: bool) {
        let Some(name_node) = node.child_by_field_name("name") else {
            return;
        };
        let name = self.text(&name_node).to_string();
        let (start_line, end_line, start_byte, end_byte) = span.unwrap_or((
            node.start_position().row + 1,
            node.end_position().row + 1,
            node.start_byte(),
            node.end_byte(),
        ));

        let exported = escapes || self.is_public_module_name(&name) || self.in_class_body();
        if escapes {
            self.push_ref(&name_node, &name, None, RefKind::Value, RefContext::Call);
        }
        let entity = Entity::new(
            &self.rel_path,
            self.abs_path.clone(),
            &name,
            SymbolKind::Class,
            start_line,
            end_line,
            start_byte,
            end_byte,
            exported,
            self.current_symbol(),
        );
        let id = entity.id.clone();
        self.entities.push(entity);

        if let Some(supers) = node.child_by_field_name("superclasses") {
            self.record_heritage(&supers, &id);
            // Base expressions are evaluated in the enclosing scope.
            self.visit_children(&supers);
        }

        self.symbol_stack.push(id);
        self.class_exported.push(exported);
        if let Some(body) = node.child_by_field_name("body") {
            self.visit_children(&body);
        }
        self.class_exported.pop();
        self.symbol_stack.pop();
    }

    /// Records `class D(Base)` pairs. Subscripted and keyword bases
    /// (`Generic[T]`, `metaclass=Meta`) carry no single resolvable base name and
    /// are skipped rather than guessed at.
    fn record_heritage(&mut self, supers: &Node, class_id: &str) {
        let mut cursor = supers.walk();
        let children: Vec<Node> = supers.children(&mut cursor).collect();
        for child in &children {
            let base = match child.kind() {
                "identifier" => Some(self.text(child).to_string()),
                // `module.Base` resolves by its final segment.
                "attribute" => child
                    .child_by_field_name("attribute")
                    .map(|a| self.text(&a).to_string()),
                _ => None,
            };
            if let Some(base) = base {
                if !base.is_empty() {
                    self.heritage.push((class_id.to_string(), base));
                }
            }
        }
    }

    /// Parameter *names* bind locals rather than referencing symbols; their
    /// annotations and default values are ordinary expressions.
    fn visit_parameters(&mut self, node: &Node) {
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in &children {
            match child.kind() {
                "identifier" => {}
                "typed_parameter" | "typed_default_parameter" => {
                    if let Some(ty) = child.child_by_field_name("type") {
                        self.record_param_type(child, &ty);
                        self.visit_type(&ty);
                    }
                    if let Some(value) = child.child_by_field_name("value") {
                        self.visit(&value);
                    }
                }
                "default_parameter" => {
                    if let Some(value) = child.child_by_field_name("value") {
                        self.visit(&value);
                    }
                }
                "list_splat_pattern" | "dictionary_splat_pattern" => {}
                _ => self.visit(child),
            }
        }
    }

    /// `def f(cfg: Config)` lets `cfg.method()` resolve to `Config.method`.
    fn record_param_type(&mut self, param: &Node, ty: &Node) {
        let Some(name_node) = param.child_by_field_name("name") else {
            return;
        };
        let name = self.text(&name_node).to_string();
        if let Some(class) = self.simple_type_name(ty) {
            self.var_types.push((name, class));
        }
    }

    /// Annotations are type-position references.
    fn visit_type(&mut self, node: &Node) {
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in &children {
            self.visit_type_inner(child);
        }
    }

    fn visit_type_inner(&mut self, node: &Node) {
        match node.kind() {
            "identifier" => {
                let name = self.text(node).to_string();
                self.push_ref(node, &name, None, RefKind::Type, RefContext::TypePosition);
            }
            "attribute" => {
                if let Some(attr) = node.child_by_field_name("attribute") {
                    let name = self.text(&attr).to_string();
                    self.push_ref(&attr, &name, None, RefKind::Type, RefContext::TypePosition);
                }
            }
            // A quoted annotation is a whole type expression, not just a bare
            // forward reference: `"Iterable[Callable[Concatenate[_T, _P], _T]]"`
            // uses four names, and reading only single-identifier strings left
            // type variables looking unused.
            "string" => {
                if let Some(inner) = self.string_value(node) {
                    for name in identifier_tokens(&inner) {
                        self.push_ref(node, &name, None, RefKind::Type, RefContext::TypePosition);
                    }
                }
            }
            _ => {
                let mut cursor = node.walk();
                let children: Vec<Node> = node.children(&mut cursor).collect();
                for child in &children {
                    self.visit_type_inner(child);
                }
            }
        }
    }

    fn visit_assignment(&mut self, node: &Node) {
        let left = node.child_by_field_name("left");
        let right = node.child_by_field_name("right");
        let annotation = node.child_by_field_name("type");

        // Module- and class-level bindings are declarations. Function locals are
        // deliberately not entities: an unused local is a linter's concern, and
        // emitting them would flood findings with non-removable noise.
        if let Some(left) = left {
            let declare = self.at_module_level() || self.in_class_body();
            if left.kind() == "identifier" {
                let name = self.text(&left).to_string();
                if declare && name != "__all__" {
                    let exported = if self.in_class_body() {
                        self.class_exported.last().copied().unwrap_or(false)
                            && !name.starts_with('_')
                    } else {
                        self.is_public_module_name(&name)
                    };
                    let entity = Entity::new(
                        &self.rel_path,
                        self.abs_path.clone(),
                        &name,
                        SymbolKind::Variable,
                        node.start_position().row + 1,
                        node.end_position().row + 1,
                        node.start_byte(),
                        node.end_byte(),
                        exported,
                        self.current_symbol(),
                    );
                    self.entities.push(entity);
                }
                // `x = ClassName()` / `x: ClassName = ...` make `x.method()`
                // resolvable. Nothing else about the value is inferred.
                if let Some(ty) = &annotation {
                    if let Some(class) = self.simple_type_name(ty) {
                        self.var_types.push((name.clone(), class));
                    }
                } else if let Some(right) = &right {
                    if let Some(class) = self.constructed_class(right) {
                        self.var_types.push((name.clone(), class));
                    }
                }
            } else {
                // `obj.attr = ...` on something other than `self` publishes a
                // member outward (the plugin/registration pattern).
                if left.kind() == "attribute" {
                    if let Some(obj) = left.child_by_field_name("object") {
                        if self.text(&obj) != "self" && self.text(&obj) != "cls" {
                            self.publishes_members = true;
                        }
                    }
                }
                // Assignment targets still read their bases and subscripts.
                self.visit(&left);
            }
        }

        if let Some(ty) = annotation {
            self.visit_type(&ty);
        }
        if let Some(right) = right {
            self.visit(&right);
        }
    }

    fn visit_call(&mut self, node: &Node) {
        let Some(func) = node.child_by_field_name("function") else {
            self.visit_children(node);
            return;
        };
        match func.kind() {
            "identifier" => {
                let name = self.text(&func).to_string();
                self.push_ref(&func, &name, None, RefKind::Value, RefContext::Call);
            }
            "attribute" => {
                if let Some(attr) = func.child_by_field_name("attribute") {
                    let name = self.text(&attr).to_string();
                    let base = func
                        .child_by_field_name("object")
                        .map(|o| self.text(&o).to_string());
                    let context =
                        if base.as_deref() == Some("self") || base.as_deref() == Some("cls") {
                            RefContext::ThisMethod
                        } else {
                            RefContext::Member
                        };
                    self.push_ref(&attr, &name, base, RefKind::Value, context);
                }
                // The receiver expression itself is a reference (`mod.fn()`
                // reads `mod`).
                if let Some(obj) = func.child_by_field_name("object") {
                    self.visit(&obj);
                }
            }
            _ => self.visit(&func),
        }

        self.record_dynamic_call(node, &func);

        if let Some(args) = node.child_by_field_name("arguments") {
            // `typing.cast("SomeType", value)` and `TypeVar(..., bound="T")`
            // carry type expressions as strings, so the names in them are real
            // type references even though no annotation position is involved.
            let callee = self.text(&func);
            let names_types_in_strings = matches!(callee, "cast" | "TypeVar" | "NewType")
                || callee.ends_with(".cast")
                || callee.ends_with(".TypeVar")
                || callee.ends_with(".NewType");
            if names_types_in_strings {
                let mut cursor = args.walk();
                let children: Vec<Node> = args.children(&mut cursor).collect();
                for child in &children {
                    if child.kind() == "string" {
                        self.visit_type_inner(child);
                    }
                }
            }
            self.visit_children(&args);
        }
    }

    /// Reflective entry points defeat static reachability; record them so the
    /// report can say why confidence is capped instead of silently guessing.
    fn record_dynamic_call(&mut self, call: &Node, func: &Node) {
        let callee = self.text(func);
        let line = self.line(call);
        let detail = match callee {
            "getattr" | "setattr" | "hasattr" | "delattr" => {
                Some("reflective attribute access".to_string())
            }
            "eval" | "exec" | "compile" => Some(format!("`{}` evaluates code at runtime", callee)),
            "__import__" => Some("`__import__` dynamic module load".to_string()),
            "globals" | "locals" | "vars" => Some("namespace introspection".to_string()),
            _ => None,
        };
        // Every reflective module load: `importlib.import_module(...)` and the
        // `__import__(...)` builtin behind it.
        let is_dynamic_import = callee == "__import__" || callee.ends_with("import_module");

        if is_dynamic_import {
            let args = call.child_by_field_name("arguments");
            let literal = args.as_ref().and_then(|args| self.first_string_arg(args));
            match literal {
                // A fully static `import_module("pkg.mod")` is an ordinary edge.
                Some(spec) => {
                    self.push_import(&spec, "*dynamic*", "*", ImportKind::DynamicLiteral, line);
                }
                None => {
                    self.note_dynamic(line, &format!("`{}` with a computed module name", callee));
                    // A computed name still fixes the package it loads from:
                    // `f"app.plugins.{n}"` and `"app.plugins." + n` both keep
                    // everything under `app/plugins` reachable.
                    if let Some(prefix) = args.and_then(|args| self.static_prefix_arg(&args)) {
                        if !self.dynamic_prefixes.contains(&prefix) {
                            self.dynamic_prefixes.push(prefix);
                        }
                    }
                }
            }
        }
        if let Some(detail) = detail {
            self.note_dynamic(line, &detail);
        }
    }

    fn note_dynamic(&mut self, line: usize, detail: &str) {
        let entry = format!("{}:{}: {}", self.rel_path, line, detail);
        if !self.dynamic_details.contains(&entry) {
            self.dynamic_details.push(entry);
        }
    }

    fn visit_attribute(&mut self, node: &Node) {
        if let Some(attr) = node.child_by_field_name("attribute") {
            let name = self.text(&attr).to_string();
            let base = node
                .child_by_field_name("object")
                .map(|o| self.text(&o).to_string());
            let context = if base.as_deref() == Some("self") || base.as_deref() == Some("cls") {
                RefContext::ThisMethod
            } else {
                RefContext::Member
            };
            self.push_ref(&attr, &name, base, RefKind::Value, context);
        }
        if let Some(obj) = node.child_by_field_name("object") {
            self.visit(&obj);
        }
    }

    fn visit_import(&mut self, node: &Node) {
        let line = self.line(node);
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in &children {
            match child.kind() {
                "dotted_name" => {
                    let spec = self.text(child).to_string();
                    // `import a.b` binds only the top-level name `a`.
                    let local = spec.split('.').next().unwrap_or(&spec).to_string();
                    self.push_import(&spec, &local, "*", ImportKind::Namespace, line);
                }
                "aliased_import" => {
                    let name = child.child_by_field_name("name");
                    let alias = child.child_by_field_name("alias");
                    if let (Some(name), Some(alias)) = (name, alias) {
                        let spec = self.text(&name).to_string();
                        let local = self.text(&alias).to_string();
                        self.push_import(&spec, &local, "*", ImportKind::Namespace, line);
                    }
                }
                _ => {}
            }
        }
    }

    fn visit_import_from(&mut self, node: &Node) {
        let line = self.line(node);
        let module = node.child_by_field_name("module_name");
        // `relative_import` text already carries its leading dots (`..pkg`).
        let base = module
            .map(|m| self.text(&m).to_string())
            .unwrap_or_default();

        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();

        if children.iter().any(|c| c.kind() == "wildcard_import") {
            self.push_import(&base, "*wildcard*", "*", ImportKind::Namespace, line);
            if self.is_init {
                // `from .mod import *` in a package `__init__` republishes that
                // module's surface as the package's surface.
                self.reexports.push(ReExportRec {
                    from_file: self.rel_path.clone(),
                    source_raw: base.clone(),
                    resolved_file: None,
                    original_name: "*".to_string(),
                    exported_name: "*".to_string(),
                    is_type_only: false,
                    is_wildcard: true,
                    line,
                });
            }
            return;
        }

        let module_id = module.map(|m| m.id());
        for child in &children {
            if Some(child.id()) == module_id {
                continue;
            }
            let (original, local) = match child.kind() {
                "dotted_name" => {
                    let name = self.text(child).to_string();
                    (name.clone(), name)
                }
                "aliased_import" => {
                    let name = child.child_by_field_name("name");
                    let alias = child.child_by_field_name("alias");
                    match (name, alias) {
                        (Some(name), Some(alias)) => {
                            (self.text(&name).to_string(), self.text(&alias).to_string())
                        }
                        _ => continue,
                    }
                }
                _ => continue,
            };
            // Stored as the full dotted candidate: `from .pkg import mod` may
            // mean the submodule `pkg/mod.py`, and the resolver falls back to
            // `pkg` itself when it does not.
            let spec = join_spec(&base, &original);
            self.push_import(&spec, &local, &original, ImportKind::Named, line);

            if self.is_init {
                // Importing a name into `__init__.py` is how a package exposes
                // it; treating it as private would report the whole public API
                // as dead.
                self.reexports.push(ReExportRec {
                    from_file: self.rel_path.clone(),
                    source_raw: spec,
                    resolved_file: None,
                    original_name: original,
                    exported_name: local,
                    is_type_only: false,
                    is_wildcard: false,
                    line,
                });
            }
        }
    }

    fn push_import(
        &mut self,
        spec: &str,
        local: &str,
        original: &str,
        kind: ImportKind,
        line: usize,
    ) {
        if spec.is_empty() {
            return;
        }
        self.imports.push(ImportRec {
            from_file: self.rel_path.clone(),
            source_raw: spec.to_string(),
            resolved_file: None,
            local_name: local.to_string(),
            original_name: original.to_string(),
            // Python type-only imports live under `if TYPE_CHECKING:`; the
            // distinction does not change reachability here.
            is_type_only: false,
            kind,
            line,
        });
    }

    /// `__all__` is the declared `import *` surface. It is additive only: a name
    /// missing from `__all__` is still importable, so it is never used to demote
    /// a public name to private.
    fn collect_all_names(&mut self, root: &Node) {
        let mut stack = vec![*root];
        while let Some(node) = stack.pop() {
            let is_all_assign = matches!(node.kind(), "assignment" | "augmented_assignment")
                && node
                    .child_by_field_name("left")
                    .map(|l| self.text(&l) == "__all__")
                    .unwrap_or(false);
            if is_all_assign {
                if let Some(right) = node.child_by_field_name("right") {
                    self.collect_string_items(&right);
                }
            }
            // `__all__.extend([...])` / `__all__.append("x")`.
            if node.kind() == "call" {
                let is_all_method = node
                    .child_by_field_name("function")
                    .and_then(|f| f.child_by_field_name("object"))
                    .map(|o| self.text(&o) == "__all__")
                    .unwrap_or(false);
                if is_all_method {
                    if let Some(args) = node.child_by_field_name("arguments") {
                        self.collect_string_items(&args);
                    }
                }
            }
            let mut cursor = node.walk();
            for child in node.children(&mut cursor) {
                stack.push(child);
            }
        }
    }

    fn collect_string_items(&mut self, node: &Node) {
        if node.kind() == "string" {
            if let Some(value) = self.string_value(node) {
                if !self.all_names.contains(&value) {
                    self.all_names.push(value);
                }
            }
            return;
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in &children {
            self.collect_string_items(child);
        }
    }

    /// A module-level name is importable unless underscore-prefixed; `__all__`
    /// membership makes it public regardless.
    ///
    /// Module dunders are the exception to the underscore rule: `__version__`,
    /// `__author__` and friends are the documented way a package publishes its
    /// metadata (`httpie.__author__`), so they are public despite the leading
    /// underscores and are read by tooling that no import edge records.
    fn is_public_module_name(&self, name: &str) -> bool {
        if !self.at_module_level() {
            return false;
        }
        self.all_names.iter().any(|n| n == name) || is_dunder(name) || !name.starts_with('_')
    }

    /// Records `__all__` entries as the file's declared export list, matching
    /// how `export { a, b }` is recorded for ES modules.
    fn finish_public_surface(&mut self) {
        let all_names = self.all_names.clone();
        for name in &all_names {
            let line = self
                .entities
                .iter()
                .find(|e| &e.name == name && e.parent.is_none())
                .map(|e| e.start_line)
                .unwrap_or(1);
            self.local_exports.push(LocalExport {
                local_name: name.clone(),
                exported_name: name.clone(),
                is_default: false,
                is_type_only: false,
                line,
            });
        }
        for entity in &mut self.entities {
            if entity.parent.is_none() && all_names.iter().any(|n| n == &entity.name) {
                entity.exported = true;
            }
        }
    }

    /// Text-level sweep for reflective patterns the tree walk can miss (e.g.
    /// inside f-strings or comprehensions).
    fn detect_dynamic_heuristics(&mut self) {
        let mut extra: Vec<String> = Vec::new();
        for (i, line) in self.source.lines().enumerate() {
            let n = i + 1;
            let t = line.trim();
            if t.starts_with('#') {
                continue;
            }
            if t.contains("__subclasses__") {
                extra.push(format!(
                    "{}:{}: `__subclasses__` registry walk",
                    self.rel_path, n
                ));
            }
            if t.contains("__getattr__") || t.contains("__getattribute__") {
                extra.push(format!(
                    "{}:{}: dynamic attribute protocol",
                    self.rel_path, n
                ));
            }
            if t.contains("pkgutil.iter_modules") || t.contains("pkgutil.walk_packages") {
                extra.push(format!("{}:{}: package auto-discovery", self.rel_path, n));
            }
            if t.contains("entry_points(") {
                extra.push(format!(
                    "{}:{}: entry-point plugin discovery",
                    self.rel_path, n
                ));
            }
        }
        for d in extra {
            if !self.dynamic_details.contains(&d) {
                self.dynamic_details.push(d);
            }
        }
    }

    /// `ClassName()` / `mod.ClassName()` in value position. Only a bare
    /// capitalised callee is treated as a constructor: Python has no `new`, so
    /// anything else is indistinguishable from a plain function call.
    fn constructed_class(&self, node: &Node) -> Option<String> {
        if node.kind() != "call" {
            return None;
        }
        let func = node.child_by_field_name("function")?;
        let name = match func.kind() {
            "identifier" => self.text(&func).to_string(),
            "attribute" => self
                .text(&func.child_by_field_name("attribute")?)
                .to_string(),
            _ => return None,
        };
        starts_uppercase(&name).then_some(name)
    }

    /// The single class name in an annotation, if it is unambiguous. Generic and
    /// union annotations name several types and resolve to none of them.
    fn simple_type_name(&self, ty: &Node) -> Option<String> {
        let inner = if ty.kind() == "type" {
            ty.named_child(0)?
        } else {
            *ty
        };
        match inner.kind() {
            "identifier" => {
                let name = self.text(&inner).to_string();
                starts_uppercase(&name).then_some(name)
            }
            "attribute" => {
                let name = self
                    .text(&inner.child_by_field_name("attribute")?)
                    .to_string();
                starts_uppercase(&name).then_some(name)
            }
            // `"Model"` forward reference.
            "string" => {
                let value = self.string_value(&inner)?;
                let name = value.trim().to_string();
                (is_identifier(&name) && starts_uppercase(&name)).then_some(name)
            }
            _ => None,
        }
    }

    fn first_string_arg(&self, args: &Node) -> Option<String> {
        let mut cursor = args.walk();
        let children: Vec<Node> = args.children(&mut cursor).collect();
        for child in &children {
            if child.kind() == "string" {
                return self.string_value(child);
            }
        }
        None
    }

    /// Directory fixed by a computed module path, as a repo-relative path.
    ///
    /// Covers both ways a package prefix is written: interpolation
    /// (`f"app.plugins.{name}"`) and concatenation (`"app.plugins." + name`), so
    /// the literal is searched for anywhere in the argument rather than only as
    /// the whole argument.
    fn static_prefix_arg(&self, args: &Node) -> Option<String> {
        let text = self.first_string_literal_text(args)?;
        let body = text.trim_start_matches(|c: char| c.is_ascii_alphabetic());
        let body = body.trim_matches(|c| c == '"' || c == '\'');
        // Everything before the first substitution is static; `%` and `{}`
        // formatting both end the static head.
        let head = body.split('{').next().unwrap_or("");
        let head = head.split('%').next().unwrap_or("");
        // A trailing dot means further segments follow, so the head names a
        // package directory rather than a module.
        let trailing_dot = head.ends_with('.');
        let head = head.trim_end_matches('.');
        let level = head.chars().take_while(|c| *c == '.').count();
        let rest = &head[level..];

        if level > 0 {
            // Relative spec: resolve against the importing module's package.
            let mut dir: Vec<&str> = self.rel_path.split('/').collect();
            dir.pop();
            for _ in 1..level {
                dir.pop()?;
            }
            let mut path = dir.join("/");
            if !rest.is_empty() {
                if !path.is_empty() {
                    path.push('/');
                }
                path.push_str(&rest.replace('.', "/"));
            }
            return (!path.is_empty()).then_some(path);
        }
        if !trailing_dot && !rest.contains('.') {
            return None;
        }
        (!rest.is_empty()).then(|| rest.replace('.', "/"))
    }

    /// Text of the first string literal anywhere inside `node`.
    fn first_string_literal_text(&self, node: &Node) -> Option<&'a str> {
        if node.kind() == "string" {
            return Some(self.text(node));
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in &children {
            if let Some(found) = self.first_string_literal_text(child) {
                return Some(found);
            }
        }
        None
    }

    /// Literal value of a string node, or `None` for f-strings (interpolated,
    /// so not a static literal).
    fn string_value(&self, node: &Node) -> Option<String> {
        if node.kind() != "string" {
            return None;
        }
        let raw = self.text(node);
        let body = raw.trim_start_matches(|c: char| c.is_ascii_alphabetic());
        let prefix = &raw[..raw.len() - body.len()];
        if prefix.to_ascii_lowercase().contains('f') {
            return None;
        }
        for quote in ["\"\"\"", "'''", "\"", "'"] {
            if let Some(rest) = body.strip_prefix(quote) {
                if let Some(inner) = rest.strip_suffix(quote) {
                    return Some(inner.to_string());
                }
            }
        }
        None
    }
}

/// Resolves a Python specifier to a repo file.
///
/// Relative specifiers (`.mod`, `..pkg.mod`) are resolved against the importing
/// file's package. Absolute ones are tried against each plausible `sys.path`
/// root — the repo root, a `src/` layout, and each ancestor directory of the
/// importing file — which covers flat, src- and monorepo layouts without
/// reading configuration.
///
/// Because `from pkg import thing` cannot be distinguished from
/// `from pkg import submodule` syntactically, specifiers arrive as the full
/// dotted candidate and the last segment is dropped if the longer path does not
/// exist.
pub fn resolve_python_import(
    from_file: &str,
    spec: &str,
    file_set: &HashSet<String>,
) -> Option<String> {
    let level = spec.chars().take_while(|c| *c == '.').count();
    let module = &spec[level..];
    let parts: Vec<&str> = module.split('.').filter(|s| !s.is_empty()).collect();

    let resolved = if level > 0 {
        let mut dir: Vec<&str> = from_file.split('/').collect();
        dir.pop();
        // `.x` is the current package; each extra dot climbs one more level.
        for _ in 1..level {
            dir.pop()?;
        }
        lookup_with_fallback(&dir.join("/"), &parts, file_set)
    } else {
        let mut roots: Vec<String> = vec![String::new(), "src".to_string()];
        // Ancestors of the importing file, most specific first: a package under
        // `packages/foo/src/` resolves its own top-level imports.
        let mut dir: Vec<&str> = from_file.split('/').collect();
        dir.pop();
        while !dir.is_empty() {
            roots.push(dir.join("/"));
            dir.pop();
        }
        let mut seen: HashSet<String> = HashSet::new();
        let mut found = None;
        for root in roots {
            if !seen.insert(root.clone()) {
                continue;
            }
            if let Some(hit) = lookup_with_fallback(&root, &parts, file_set) {
                found = Some(hit);
                break;
            }
        }
        found
    };

    // A file importing from its own package resolves to itself for
    // `from . import x` when `x` is a plain attribute; that self-edge carries no
    // information and would show up as a file depending on itself.
    resolved.filter(|hit| hit != from_file)
}

/// The `__init__.py` files the interpreter executes as a side effect of
/// importing `target`. Importing `a.b.c` imports `a` then `a.b` first, so those
/// package initialisers run whether or not anything names them — a fact, not a
/// heuristic, and the reason a package `__init__.py` is not dead merely because
/// no module imports it by name.
pub fn package_init_chain(target: &str, file_set: &HashSet<String>) -> Vec<String> {
    let mut out = Vec::new();
    let parts: Vec<&str> = target.split('/').collect();
    // Every ancestor directory of the target is a package whose initialiser runs.
    for depth in 1..parts.len() {
        let candidate = format!("{}/__init__.py", parts[..depth].join("/"));
        if candidate != target && file_set.contains(&candidate) {
            out.push(candidate);
        }
    }
    out
}

fn lookup_with_fallback(base: &str, parts: &[&str], file_set: &HashSet<String>) -> Option<String> {
    if parts.is_empty() {
        return lookup(base, parts, file_set);
    }
    lookup(base, parts, file_set).or_else(|| lookup(base, &parts[..parts.len() - 1], file_set))
}

fn lookup(base: &str, parts: &[&str], file_set: &HashSet<String>) -> Option<String> {
    let mut path = String::new();
    if !base.is_empty() {
        path.push_str(base);
    }
    for part in parts {
        if !path.is_empty() {
            path.push('/');
        }
        path.push_str(part);
    }
    if path.is_empty() {
        return None;
    }
    // A dotted name may be a module, a package directory, or a stub.
    [
        format!("{}.py", path),
        format!("{}/__init__.py", path),
        format!("{}.pyi", path),
    ]
    .into_iter()
    .find(|candidate| file_set.contains(candidate))
}

fn join_spec(base: &str, name: &str) -> String {
    if base.is_empty() {
        name.to_string()
    } else if base.ends_with('.') {
        format!("{}{}", base, name)
    } else {
        format!("{}.{}", base, name)
    }
}

/// `if __name__ == "__main__":` — the file is meant to be executed directly.
fn has_main_guard(source: &str) -> bool {
    source.lines().any(|line| {
        let t = line.trim();
        t.starts_with("if ")
            && t.contains("__name__")
            && (t.contains("\"__main__\"") || t.contains("'__main__'"))
    })
}

fn contains_call(node: &Node) -> bool {
    if node.kind() == "call" {
        return true;
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    children.iter().any(contains_call)
}

/// Language- and stdlib-level decorators that *transform* a definition and
/// rebind the result to the same name, rather than handing it to something that
/// may call it later. Because they publish nothing, a definition carrying only
/// these is still judged by ordinary reachability — an unused `@staticmethod`
/// remains reportable.
///
/// This set is deliberately limited to Python and its standard library: it is
/// language semantics, not a list of frameworks to keep up to date. Any other
/// decorator means the definition escapes to an unknown callable.
fn is_transformer_decorator(name: &str) -> bool {
    // `@x.setter` / `@x.getter` / `@x.deleter` complete a `property`.
    if let Some((_, last)) = name.rsplit_once('.') {
        if matches!(last, "setter" | "getter" | "deleter") {
            return true;
        }
        return is_transformer_decorator(last);
    }
    matches!(
        name,
        "staticmethod"
            | "classmethod"
            | "property"
            | "cached_property"
            | "abstractmethod"
            | "abstractproperty"
            | "override"
            | "overload"
            | "final"
            | "dataclass"
            | "contextmanager"
            | "asynccontextmanager"
            | "lru_cache"
            | "cache"
            | "wraps"
            | "singledispatch"
            | "singledispatchmethod"
            | "total_ordering"
            | "runtime_checkable"
            | "no_type_check"
    )
}

fn is_dunder(name: &str) -> bool {
    name.starts_with("__") && name.ends_with("__") && name.len() > 4
}

/// Whether a name follows the CapWords class convention, ignoring the leading
/// underscores that mark a class private (`_Registry`). Python has no `new`, so
/// this convention is the only signal distinguishing `x = Thing()` from an
/// ordinary function call.
fn starts_uppercase(name: &str) -> bool {
    name.trim_start_matches('_')
        .chars()
        .next()
        .is_some_and(|c| c.is_uppercase())
}

/// Identifier-like tokens inside a quoted type expression. Splitting on
/// everything else turns `"Iterable[Callable[Concatenate[_T, _P], _T]]"` into the
/// names it actually uses.
fn identifier_tokens(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current = String::new();
    for c in text.chars() {
        if c.is_alphanumeric() || c == '_' {
            current.push(c);
        } else if !current.is_empty() {
            out.push(std::mem::take(&mut current));
        }
    }
    if !current.is_empty() {
        out.push(current);
    }
    out.retain(|t| is_identifier(t));
    out
}

fn is_identifier(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .next()
            .is_some_and(|c| c.is_alphabetic() || c == '_')
        && s.chars().all(|c| c.is_alphanumeric() || c == '_')
}

/// Builtins and soft keywords that can never name a repo symbol in value
/// position. Kept to true builtins: anything a project could plausibly define
/// itself is left in so a real definition is still matched.
fn is_python_builtin(name: &str) -> bool {
    matches!(
        name,
        "abs" | "aiter"
            | "all"
            | "anext"
            | "any"
            | "ascii"
            | "bin"
            | "bool"
            | "breakpoint"
            | "bytearray"
            | "bytes"
            | "callable"
            | "chr"
            | "classmethod"
            | "compile"
            | "complex"
            | "delattr"
            | "dict"
            | "dir"
            | "divmod"
            | "enumerate"
            | "eval"
            | "exec"
            | "filter"
            | "float"
            | "format"
            | "frozenset"
            | "getattr"
            | "globals"
            | "hasattr"
            | "hash"
            | "help"
            | "hex"
            | "id"
            | "input"
            | "int"
            | "isinstance"
            | "issubclass"
            | "iter"
            | "len"
            | "list"
            | "locals"
            | "map"
            | "max"
            | "memoryview"
            | "min"
            | "next"
            | "object"
            | "oct"
            | "open"
            | "ord"
            | "pow"
            | "print"
            | "property"
            | "range"
            | "repr"
            | "reversed"
            | "round"
            | "set"
            | "setattr"
            | "slice"
            | "sorted"
            | "staticmethod"
            | "str"
            | "sum"
            | "super"
            | "tuple"
            | "type"
            | "vars"
            | "zip"
            | "__import__"
            // Bindings, not references.
            | "self"
            | "cls"
            | "None"
            | "True"
            | "False"
            | "NotImplemented"
            | "Ellipsis"
            // Module dunders.
            | "__name__"
            | "__file__"
            | "__doc__"
            | "__all__"
            | "__dict__"
            | "__class__"
            | "__module__"
            // Built-in exception hierarchy.
            | "BaseException"
            | "Exception"
            | "ArithmeticError"
            | "AssertionError"
            | "AttributeError"
            | "EOFError"
            | "FileExistsError"
            | "FileNotFoundError"
            | "FloatingPointError"
            | "GeneratorExit"
            | "ImportError"
            | "IndentationError"
            | "IndexError"
            | "InterruptedError"
            | "IsADirectoryError"
            | "KeyError"
            | "KeyboardInterrupt"
            | "LookupError"
            | "MemoryError"
            | "ModuleNotFoundError"
            | "NameError"
            | "NotADirectoryError"
            | "NotImplementedError"
            | "OSError"
            | "OverflowError"
            | "PermissionError"
            | "RecursionError"
            | "ReferenceError"
            | "RuntimeError"
            | "StopAsyncIteration"
            | "StopIteration"
            | "SyntaxError"
            | "SystemError"
            | "SystemExit"
            | "TimeoutError"
            | "TypeError"
            | "UnboundLocalError"
            | "UnicodeDecodeError"
            | "UnicodeEncodeError"
            | "UnicodeError"
            | "ValueError"
            | "Warning"
            | "DeprecationWarning"
            | "UserWarning"
            | "ZeroDivisionError"
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files(paths: &[&str]) -> HashSet<String> {
        paths.iter().map(|p| p.to_string()).collect()
    }

    #[test]
    fn resolves_absolute_module_and_package() {
        let set = files(&[
            "main.py",
            "pkg/__init__.py",
            "pkg/mod.py",
            "pkg/sub/__init__.py",
        ]);
        assert_eq!(
            resolve_python_import("main.py", "pkg.mod", &set).as_deref(),
            Some("pkg/mod.py")
        );
        // A dotted name may name a package directory instead of a module.
        assert_eq!(
            resolve_python_import("main.py", "pkg.sub", &set).as_deref(),
            Some("pkg/sub/__init__.py")
        );
        // Third-party and stdlib modules are not repo files.
        assert_eq!(resolve_python_import("main.py", "os.path", &set), None);
    }

    /// `from pkg import thing` arrives as the full dotted candidate, so the last
    /// segment is dropped when it names an attribute rather than a submodule.
    #[test]
    fn falls_back_to_parent_module_for_attribute_imports() {
        let set = files(&["app/__init__.py", "app/models.py", "app/models/deep.py"]);
        assert_eq!(
            resolve_python_import("app/__init__.py", ".models.User", &set).as_deref(),
            Some("app/models.py")
        );
        // When the longer path does exist it wins: it is the real submodule.
        assert_eq!(
            resolve_python_import("app/__init__.py", ".models.deep", &set).as_deref(),
            Some("app/models/deep.py")
        );
    }

    #[test]
    fn resolves_relative_levels() {
        let set = files(&[
            "app/__init__.py",
            "app/util.py",
            "app/sub/mod.py",
            "app/sub/deep/leaf.py",
        ]);
        // One dot is the current package.
        assert_eq!(
            resolve_python_import("app/sub/mod.py", ".deep.leaf", &set).as_deref(),
            Some("app/sub/deep/leaf.py")
        );
        // Two dots climb to the parent package.
        assert_eq!(
            resolve_python_import("app/sub/mod.py", "..util", &set).as_deref(),
            Some("app/util.py")
        );
        // Climbing past the repository root resolves to nothing.
        assert_eq!(
            resolve_python_import("app/sub/mod.py", "....util", &set),
            None
        );
    }

    /// `from . import x` becomes `.x`: a submodule if one exists, otherwise the
    /// package initialiser — and never the importing file itself.
    #[test]
    fn resolves_package_relative_imports_without_self_edges() {
        let set = files(&["app/__init__.py", "app/util.py"]);
        assert_eq!(
            resolve_python_import("app/__init__.py", ".util", &set).as_deref(),
            Some("app/util.py")
        );
        // `from . import CONSTANT` would resolve back to __init__.py itself.
        assert_eq!(
            resolve_python_import("app/__init__.py", ".CONSTANT", &set),
            None
        );
    }

    #[test]
    fn resolves_src_and_monorepo_layouts() {
        let set = files(&["src/app/__init__.py", "src/app/mod.py"]);
        assert_eq!(
            resolve_python_import("src/app/__init__.py", "app.mod", &set).as_deref(),
            Some("src/app/mod.py")
        );
        let mono = files(&[
            "packages/svc/src/svc/__init__.py",
            "packages/svc/src/svc/core.py",
        ]);
        assert_eq!(
            resolve_python_import("packages/svc/src/svc/__init__.py", "svc.core", &mono).as_deref(),
            Some("packages/svc/src/svc/core.py")
        );
    }

    /// Importing `a.b.c` executes every ancestor package initialiser.
    #[test]
    fn package_init_chain_lists_ancestor_initialisers() {
        let set = files(&["a/__init__.py", "a/b/__init__.py", "a/b/c.py"]);
        assert_eq!(
            package_init_chain("a/b/c.py", &set),
            vec!["a/__init__.py".to_string(), "a/b/__init__.py".to_string()]
        );
        // A package initialiser does not list itself.
        assert_eq!(
            package_init_chain("a/b/__init__.py", &set),
            vec!["a/__init__.py".to_string()]
        );
        // Only initialisers that exist are reported (namespace packages).
        assert!(package_init_chain("a/b/c.py", &files(&["a/b/c.py"])).is_empty());
    }

    #[test]
    fn transformer_decorators_are_recognised_through_attributes() {
        assert!(is_transformer_decorator("staticmethod"));
        assert!(is_transformer_decorator("functools.lru_cache"));
        assert!(is_transformer_decorator("name.setter"));
        // Anything else receives the definition and may call it later.
        assert!(!is_transformer_decorator("app.route"));
        assert!(!is_transformer_decorator("pytest.fixture"));
        assert!(!is_transformer_decorator("celery.task"));
    }
}
