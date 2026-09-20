use crate::imports::{ImportKind, ImportRec, ReExportRec, RefContext, RefKind, ReferenceRec};
use crate::symbols::{Entity, SymbolKind};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use tree_sitter::{Node, Parser};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Language {
    TypeScript,
    Tsx,
    JavaScript,
    Jsx,
}

impl Language {
    pub fn from_extension(ext: &str, file_name: &str) -> Option<Self> {
        // .d.ts is TypeScript declaration — still parse with TS grammar.
        if file_name.ends_with(".d.ts") {
            return Some(Language::TypeScript);
        }
        match ext {
            "ts" => Some(Language::TypeScript),
            "tsx" => Some(Language::Tsx),
            "mts" | "cts" => Some(Language::TypeScript),
            "js" | "mjs" | "cjs" => Some(Language::JavaScript),
            "jsx" => Some(Language::Jsx),
            "mjsx" | "cjsx" => Some(Language::Jsx),
            _ => None,
        }
    }

    fn ts_language(&self) -> tree_sitter::Language {
        match self {
            Language::TypeScript => tree_sitter_typescript::language_typescript(),
            Language::Tsx => tree_sitter_typescript::language_tsx(),
            Language::JavaScript | Language::Jsx => tree_sitter_javascript::language(),
        }
    }
}

pub const SUPPORTED_EXTENSIONS: &[&str] = &["ts", "tsx", "js", "mjs", "cjs", "jsx", "mts", "cts"];

/// Minified artifacts (vendored bundles) are generated code: analyzing them
/// produces noise (single-letter scope on one line). Never analyze.
pub fn is_minified_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|s| s.to_str())
        .map(|n| n.contains(".min."))
        .unwrap_or(false)
}

pub fn is_supported_file(path: &Path) -> bool {
    if is_minified_file(path) {
        return false;
    }
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if file_name.ends_with(".d.ts") {
        return true;
    }
    match path.extension().and_then(|s| s.to_str()) {
        Some(ext) => SUPPORTED_EXTENSIONS.contains(&ext),
        None => false,
    }
}

pub fn detect_language(path: &Path) -> Option<Language> {
    let file_name = path.file_name().and_then(|s| s.to_str()).unwrap_or("");
    if file_name.ends_with(".d.ts") {
        return Some(Language::TypeScript);
    }
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("");
    Language::from_extension(ext, file_name)
}

#[derive(Debug, Clone)]
pub struct ParsedFile {
    pub abs_path: PathBuf,
    pub rel_path: String,
    pub language: Language,
    pub source: String,
    pub entities: Vec<Entity>,
    pub imports: Vec<ImportRec>,
    pub reexports: Vec<ReExportRec>,
    /// Local `export { a, b }` (no source) — names exported from this file.
    pub local_exports: Vec<LocalExport>,
    pub references: Vec<ReferenceRec>,
    /// Local variable -> class name (from `new X()` initializers and `: X` annotations),
    /// used to resolve `recv.method()` receivers in the same file.
    pub var_types: Vec<(String, String)>,
    /// Methods defined in object literals passed directly as call/`new` arguments
    /// (`new Readable({ read() {} })`): invoked as callbacks/hooks by the callee.
    /// Entries are (method name, start line).
    pub hook_methods: Vec<(String, usize)>,
    /// Methods of objects assigned to module exports (`module.exports = { create() {} }`,
    /// `export default { ... }`): public surface called by consumers/frameworks.
    /// Entries are (method name, start line).
    pub surface_methods: Vec<(String, usize)>,
    /// Receiver bases of computed dispatch calls (`handlers[key](...)`): any
    /// method owned by the base object may be invoked.
    /// Entries are (base name, enclosing symbol or None).
    pub dynamic_dispatch_bases: Vec<(String, Option<String>)>,
    /// Directories that may be loaded dynamically, repo-relative slash paths
    /// (`import(`./plugins/${n}`)` → `src/plugins`, `require(path.join(
    /// __dirname, "rules", name))` → that dir). Files under them are never
    /// auto-removed.
    pub dynamic_prefixes: Vec<String>,
    /// Variable names spread into object literals (`{...opts}`) in this file:
    /// their properties flow into other objects.
    pub spread_vars: Vec<String>,
    /// Class heritage: (derived class entity id, base name as written).
    /// Only plain `extends Base` / `extends ns.Base` (resolvable); mixin
    /// applications `extends M(Base)` are skipped (not statically known).
    pub heritage: Vec<(String, String)>,
    /// True when the file has no imports/exports at all: a `.d.ts` script file
    /// whose top-level types are ambient globals visible across files.
    pub is_ambient: bool,
    /// Classes constructed in `return` position or exported initializers:
    /// instances may escape to external callers.
    pub escaped_classes: Vec<String>,
    /// True when the file assigns to `obj.prop` (non-`this`, non-`module.exports`)
    /// — the plugin/prototype-registration pattern publishing outward.
    pub publishes_members: bool,
    /// True when the file is an executable script (`process.argv` or shebang):
    /// run manually, never imported as a module.
    pub is_executable_script: bool,
    /// True when top-level code (outside any declaration) performs calls:
    /// with no exports at all, the file looks like a script, not a module.
    pub has_top_level_calls: bool,
    pub dynamic_risk: bool,
    pub dynamic_details: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct LocalExport {
    pub local_name: String,
    pub exported_name: String,
    pub is_default: bool,
    pub is_type_only: bool,
    pub line: usize,
}

pub fn parse_file(root: &Path, abs_path: &Path) -> Option<ParsedFile> {
    let language = detect_language(abs_path)?;
    let source_bytes = std::fs::read(abs_path).ok()?;
    let source = String::from_utf8_lossy(&source_bytes).to_string();
    let rel_path = to_rel_slash(root, abs_path);
    parse_source(root, abs_path, &rel_path, language, &source)
}

pub fn parse_source(
    _root: &Path,
    abs_path: &Path,
    rel_path: &str,
    language: Language,
    source: &str,
) -> Option<ParsedFile> {
    let ts_lang: tree_sitter::Language = language.ts_language();
    let mut parser = Parser::new();
    parser.set_language(&ts_lang).ok()?;
    let tree = parser.parse(source, None)?;
    let root_node = tree.root_node();

    let mut visitor = Visitor {
        source,
        abs_path: abs_path.to_path_buf(),
        rel_path: rel_path.to_string(),
        entities: Vec::new(),
        imports: Vec::new(),
        reexports: Vec::new(),
        local_exports: Vec::new(),
        references: Vec::new(),
        var_types: Vec::new(),
        hook_methods: Vec::new(),
        surface_methods: Vec::new(),
        dynamic_dispatch_bases: Vec::new(),
        dynamic_prefixes: Vec::new(),
        spread_vars: Vec::new(),
        heritage: Vec::new(),
        escaped_classes: Vec::new(),
        returned_idents: Vec::new(),
        publishes_members: false,
        has_top_level_calls: false,
        is_executable_script: source.starts_with("#!")
            || source.contains("process.argv")
            || source.contains("process.execArgv"),
        dynamic_details: Vec::new(),
        symbol_stack: Vec::new(),
        declared_names_stack: Vec::new(),
    };
    visitor.visit_children(&root_node, false);
    visitor.finish_local_export_marks();
    visitor.finish_escape_tracking();
    visitor.detect_dynamic_heuristics();

    let dynamic_risk = !visitor.dynamic_details.is_empty();
    let is_ambient = visitor.imports.is_empty()
        && visitor.reexports.is_empty()
        && visitor.local_exports.is_empty();
    Some(ParsedFile {
        abs_path: abs_path.to_path_buf(),
        rel_path: rel_path.to_string(),
        language,
        source: source.to_string(),
        entities: visitor.entities,
        imports: visitor.imports,
        reexports: visitor.reexports,
        local_exports: visitor.local_exports,
        references: visitor.references,
        var_types: visitor.var_types,
        hook_methods: visitor.hook_methods,
        surface_methods: visitor.surface_methods,
        dynamic_dispatch_bases: visitor.dynamic_dispatch_bases,
        dynamic_prefixes: visitor.dynamic_prefixes,
        spread_vars: visitor.spread_vars,
        heritage: visitor.heritage,
        is_executable_script: visitor.is_executable_script,
        has_top_level_calls: visitor.has_top_level_calls,
        is_ambient,
        escaped_classes: visitor.escaped_classes,
        publishes_members: visitor.publishes_members,
        dynamic_risk,
        dynamic_details: visitor.dynamic_details,
    })
}

pub fn to_rel_slash(root: &Path, abs: &Path) -> String {
    let rel = abs.strip_prefix(root).unwrap_or(abs);
    rel.to_string_lossy().replace('\\', "/")
}

struct Visitor<'a> {
    source: &'a str,
    abs_path: PathBuf,
    rel_path: String,
    entities: Vec<Entity>,
    imports: Vec<ImportRec>,
    reexports: Vec<ReExportRec>,
    local_exports: Vec<LocalExport>,
    references: Vec<ReferenceRec>,
    var_types: Vec<(String, String)>,
    hook_methods: Vec<(String, usize)>,
    surface_methods: Vec<(String, usize)>,
    dynamic_dispatch_bases: Vec<(String, Option<String>)>,
    dynamic_prefixes: Vec<String>,
    spread_vars: Vec<String>,
    heritage: Vec<(String, String)>,
    escaped_classes: Vec<String>,
    returned_idents: Vec<String>,
    publishes_members: bool,
    is_executable_script: bool,
    has_top_level_calls: bool,
    dynamic_details: Vec<String>,
    symbol_stack: Vec<String>,
    declared_names_stack: Vec<HashSet<String>>,
}

impl<'a> Visitor<'a> {
    fn text(&self, node: &Node) -> &str {
        node.utf8_text(self.source.as_bytes()).unwrap_or("")
    }

    fn line(&self, node: &Node) -> usize {
        node.start_position().row + 1
    }

    fn current_symbol(&self) -> Option<String> {
        self.symbol_stack.last().cloned()
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
        // Skip JS keywords / builtins that can never be repo symbols in these
        // positions. Member/this property names are NOT filtered: any identifier
        // (even `type`) can be a method name, and resolution only links when the
        // receiver provably owns it.
        if context != RefContext::Member
            && context != RefContext::ThisMethod
            && is_builtin_name(name)
        {
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

    fn visit_children(&mut self, node: &Node, exported: bool) {
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in children {
            self.visit(&child, exported);
        }
    }

    fn visit(&mut self, node: &Node, exported: bool) {
        let kind = node.kind();
        match kind {
            "export_statement" => {
                self.visit_export_statement(node);
                return;
            }
            "import_statement" => {
                self.visit_import_statement(node);
                return;
            }
            "function_declaration" | "generator_function_declaration" | "function_signature" => {
                self.visit_function_decl(node, exported, SymbolKind::Function);
                return;
            }
            "class_declaration" | "abstract_class_declaration" => {
                self.visit_class_decl(node, exported);
                return;
            }
            "lexical_declaration" | "variable_declaration" => {
                self.visit_lexical(node, exported);
                return;
            }
            "interface_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = self.text(&name_node).to_string();
                    let e = Entity::new(
                        &self.rel_path,
                        self.abs_path.clone(),
                        &name,
                        SymbolKind::Interface,
                        node.start_position().row + 1,
                        node.end_position().row + 1,
                        node.start_byte(),
                        node.end_byte(),
                        exported,
                        self.symbol_stack.last().cloned(),
                    );
                    self.symbol_stack.push(e.id.clone());
                    self.entities.push(e);
                    // Visit type params/body for type refs
                    self.visit_children_typed(node);
                    self.symbol_stack.pop();
                }
                return;
            }
            "type_alias_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = self.text(&name_node).to_string();
                    let e = Entity::new(
                        &self.rel_path,
                        self.abs_path.clone(),
                        &name,
                        SymbolKind::TypeAlias,
                        node.start_position().row + 1,
                        node.end_position().row + 1,
                        node.start_byte(),
                        node.end_byte(),
                        exported,
                        self.symbol_stack.last().cloned(),
                    );
                    self.symbol_stack.push(e.id.clone());
                    self.entities.push(e);
                    self.visit_children_typed(node);
                    self.symbol_stack.pop();
                }
                return;
            }
            "enum_declaration" => {
                if let Some(name_node) = node.child_by_field_name("name") {
                    let name = self.text(&name_node).to_string();
                    let e = Entity::new(
                        &self.rel_path,
                        self.abs_path.clone(),
                        &name,
                        SymbolKind::Enum,
                        node.start_position().row + 1,
                        node.end_position().row + 1,
                        node.start_byte(),
                        node.end_byte(),
                        exported,
                        self.symbol_stack.last().cloned(),
                    );
                    self.symbol_stack.push(e.id.clone());
                    self.entities.push(e);
                    self.visit_children(node, false);
                    self.symbol_stack.pop();
                } else {
                    self.visit_children(node, false);
                }
                return;
            }
            "internal_module" | "module" | "namespace_declaration" => {
                // Bare keyword tokens (`module`, `namespace`) share these kind
                // names in some grammars — only real declarations have a name.
                let Some(name_node) = node.child_by_field_name("name") else {
                    self.visit_children(node, false);
                    return;
                };
                let name = self.text(&name_node).to_string();
                // `declare module 'pkg'` (quoted name) augments an external/public
                // module: its members are public surface.
                let is_ambient_augmentation = name.starts_with('\'') || name.starts_with('"');
                let e = Entity::new(
                    &self.rel_path,
                    self.abs_path.clone(),
                    &name,
                    SymbolKind::Namespace,
                    node.start_position().row + 1,
                    node.end_position().row + 1,
                    node.start_byte(),
                    node.end_byte(),
                    exported || is_ambient_augmentation,
                    self.symbol_stack.last().cloned(),
                );
                self.symbol_stack.push(e.id.clone());
                self.entities.push(e);
                self.visit_children(node, false);
                self.symbol_stack.pop();
                return;
            }
            "method_definition" => {
                self.visit_method(node);
                return;
            }
            // `...X` spread: X's properties flow into this object literal.
            "object" => {
                let mut cursor = node.walk();
                let members: Vec<Node> = node.children(&mut cursor).collect();
                for m in &members {
                    if m.kind() == "spread_element" {
                        let mut c2 = m.walk();
                        for inner in m.children(&mut c2) {
                            if inner.kind() == "identifier" {
                                self.spread_vars.push(self.text(&inner).to_string());
                            }
                        }
                    }
                }
                self.visit_children(node, false);
                return;
            }
            // `declare ...` is transparent: propagate the export context inward
            // (`export declare function f()` must stay exported).
            "ambient_declaration" => {
                self.visit_children(node, exported);
                return;
            }
            // Reference sites
            "call_expression" => {
                self.visit_call(node);
                // still walk args for nested refs
                self.visit_children(node, false);
                return;
            }
            "new_expression" => {
                self.visit_new(node);
                self.visit_children(node, false);
                return;
            }
            "jsx_opening_element" | "jsx_self_closing_element" => {
                self.visit_jsx_open(node);
                self.visit_children(node, false);
                return;
            }
            "member_expression" => {
                // `this.foo` handled here; other member refs recorded as member context.
                self.visit_member(node);
                self.visit_children(node, false);
                return;
            }
            "return_statement" => {
                self.visit_return(node);
                self.visit_children(node, false);
                return;
            }
            "import_expression" => {
                self.visit_import_expr(node);
                self.visit_children(node, false);
                return;
            }
            "subscript_expression" => {
                self.visit_subscript(node);
                self.visit_children(node, false);
                return;
            }
            "assignment_expression" | "augmented_assignment_expression" => {
                // Returns true when RHS children were already visited (avoid doubles).
                if !self.visit_assignment(node) {
                    self.visit_children(node, false);
                }
                return;
            }
            "type_identifier" | "generic_type" | "type_query" => {
                // Record type-position refs generically below; avoid double work.
                self.visit_type_node(node);
                self.visit_children(node, false);
                return;
            }
            _ => {}
        }

        // Generic identifier usage: `const x = foo` etc.
        if kind == "identifier" {
            if self.is_usage_identifier(node) {
                let name = self.text(node).to_string();
                self.push_ref(node, &name, None, RefKind::Value, RefContext::Identifier);
            }
            return;
        }
        // `{ foo }` object shorthand in expression position reads `foo`.
        // (Pattern-position `shorthand_property_identifier_pattern` is a binding, not a use.)
        if kind == "shorthand_property_identifier" {
            let name = self.text(node).to_string();
            self.push_ref(node, &name, None, RefKind::Value, RefContext::Identifier);
            return;
        }

        // Default: recurse.
        // Track `this` receivers inside class bodies via stack — handled in visit_member.
        self.visit_children(node, false);
    }

    fn visit_children_typed(&mut self, node: &Node) {
        // Walk children recording type_identifier refs as TYPE.
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in children {
            if child.kind() == "type_identifier" {
                let name = self.text(&child).to_string();
                if !is_builtin_name(&name) && !is_builtin_type(&name) {
                    self.push_ref(&child, &name, None, RefKind::Type, RefContext::TypePosition);
                }
            } else {
                self.visit_typed_inner(&child);
            }
        }
    }

    fn visit_typed_inner(&mut self, node: &Node) {
        if node.kind() == "type_identifier" {
            let name = self.text(node).to_string();
            if !is_builtin_name(&name) && !is_builtin_type(&name) {
                self.push_ref(node, &name, None, RefKind::Type, RefContext::TypePosition);
            }
            return;
        }
        // Computed keys (`[sym]: T`, `[K in keyof T]`) hold VALUE expressions:
        // visit them with value semantics so `[axiosResponseDefault]` links.
        if node.kind() == "computed_property_name" {
            self.visit_children(node, false);
            return;
        }
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in children {
            self.visit_typed_inner(&child);
        }
        // Also record value refs inside type bodies? e.g. typeof X — the identifier walk above covers.
    }

    fn is_usage_identifier(&self, _node: &Node) -> bool {
        // Called for bare `identifier` nodes encountered during generic recursion.
        // Declaration-name positions are never reached here because declarations
        // return early with dedicated visitors. Import/export specifiers also
        // return early. So a bare identifier here is a usage.
        // Parent-based exclusions for property positions:
        // member_expression property `obj.foo` — `foo` is property_identifier, not identifier, so fine.
        // `this` handled elsewhere.
        true
    }

    // ---- declarations ----

    fn visit_function_decl(&mut self, node: &Node, exported: bool, kind: SymbolKind) {
        // Overload signatures (`function f(x: string): void;`, no body) are not
        // removable code: skip the entity, but still visit children for type refs.
        // Calls resolve to the implementation overload.
        if node.kind() == "function_signature" {
            self.visit_children(node, false);
            return;
        }
        self.record_param_types(node);
        let name_node = node.child_by_field_name("name");
        let name = name_node
            .map(|n| self.text(&n).to_string())
            .unwrap_or_else(|| "default".to_string());
        let is_default = name == "default" || is_default_export_context(self.source, node);
        let mut e = Entity::new(
            &self.rel_path,
            self.abs_path.clone(),
            &name,
            kind,
            node.start_position().row + 1,
            node.end_position().row + 1,
            node.start_byte(),
            node.end_byte(),
            exported,
            self.symbol_stack.last().cloned(),
        );
        if is_default {
            e.is_default_export = true;
            e.exported = true;
        }
        self.symbol_stack.push(e.id.clone());
        self.declared_names_stack.push(HashSet::new());
        self.entities.push(e);
        self.visit_children(node, false);
        self.declared_names_stack.pop();
        self.symbol_stack.pop();
    }

    fn visit_class_decl(&mut self, node: &Node, exported: bool) {
        let name_node = node.child_by_field_name("name");
        let Some(name_node) = name_node else {
            // `export default class {}` — anonymous default export
            let mut e = Entity::new(
                &self.rel_path,
                self.abs_path.clone(),
                "default",
                SymbolKind::Class,
                node.start_position().row + 1,
                node.end_position().row + 1,
                node.start_byte(),
                node.end_byte(),
                true,
                None,
            );
            e.is_default_export = true;
            self.symbol_stack.push(e.id.clone());
            self.entities.push(e);
            self.visit_children(node, false);
            self.symbol_stack.pop();
            return;
        };
        let name = self.text(&name_node).to_string();
        let name_id = name_node.id();
        let is_default = is_default_export_context(self.source, node);
        let mut e = Entity::new(
            &self.rel_path,
            self.abs_path.clone(),
            &name,
            SymbolKind::Class,
            node.start_position().row + 1,
            node.end_position().row + 1,
            node.start_byte(),
            node.end_byte(),
            exported,
            self.symbol_stack.last().cloned(),
        );
        if is_default {
            e.is_default_export = true;
            e.exported = true;
        }
        let class_id = e.id.clone();
        self.symbol_stack.push(class_id.clone());
        self.entities.push(e);
        // Visit body: methods become child entities.
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in children {
            if child.kind() == "class_heritage" {
                // `extends Base` / `extends ns.Base` — record for override-aware analysis.
                // Mixin applications (`extends M(Base)`) are not statically known: skip.
                self.record_heritage(&child, &class_id);
                self.visit(&child, false);
            } else if child.kind() == "class_body" {
                let mut c2 = child.walk();
                let members: Vec<Node> = child.children(&mut c2).collect();
                for m in members {
                    if m.kind() == "method_definition" {
                        self.visit_method_in_class(&m, &class_id);
                    } else if m.kind() == "public_field_definition"
                        || m.kind() == "field_definition"
                        || m.kind() == "property_definition"
                    {
                        self.visit_class_field(&m, &class_id);
                    } else {
                        self.visit(&m, false);
                    }
                }
            } else if child.id() == name_id {
                // class name — declaration site, not a reference
                continue;
            } else {
                self.visit(&child, false);
            }
        }
        self.symbol_stack.pop();
    }

    fn visit_method(&mut self, node: &Node) {
        // Method outside class-body fast path (e.g. object literal methods) — treat as nested function ref context.
        let name_node = node.child_by_field_name("name");
        let name = name_node
            .map(|n| strip_quotes(self.text(&n)))
            .unwrap_or_else(|| "anonymous".to_string());
        let parent = self.symbol_stack.last().cloned();
        let e = Entity::new(
            &self.rel_path,
            self.abs_path.clone(),
            &name,
            SymbolKind::Method,
            node.start_position().row + 1,
            node.end_position().row + 1,
            node.start_byte(),
            node.end_byte(),
            false,
            parent,
        );
        self.symbol_stack.push(e.id.clone());
        self.entities.push(e);
        self.record_param_types(node);
        self.visit_children(node, false);
        self.symbol_stack.pop();
    }

    /// Record `extends` bases for override-aware analysis.
    /// Finds the heritage clause of a class node and records it for `class_id`.
    fn record_class_heritage(&mut self, class_node: &Node, class_id: &str) {
        let mut cursor = class_node.walk();
        let children: Vec<Node> = class_node.children(&mut cursor).collect();
        for child in &children {
            if child.kind() == "class_heritage" {
                self.record_heritage(child, class_id);
            }
        }
    }

    /// Record `extends` bases for override-aware analysis.
    fn record_heritage(&mut self, heritage_node: &Node, class_id: &str) {
        let mut cursor = heritage_node.walk();
        let children: Vec<Node> = heritage_node.children(&mut cursor).collect();
        for c in &children {
            match c.kind() {
                "identifier" => {
                    let n = self.text(c).to_string();
                    // Skip the `extends` keyword itself if lexed as identifier (it isn't,
                    // but guard anyway) — real base names only.
                    if n != "extends" {
                        self.heritage.push((class_id.to_string(), n));
                    }
                }
                "member_expression" => {
                    // `extends ns.Base` — resolve `Base` via same-file or imports.
                    let (_, prop) = member_parts(self.source, c);
                    if let Some(p) = prop {
                        self.heritage.push((class_id.to_string(), p));
                    }
                }
                _ => {}
            }
        }
    }

    fn visit_method_in_class(&mut self, node: &Node, class_id: &str) {
        let name_node = node.child_by_field_name("name");
        let name = name_node
            .map(|n| strip_quotes(self.text(&n)))
            .unwrap_or_else(|| "anonymous".to_string());
        // Skip constructor for dead-method findings? Keep it but never report constructor as dead.
        let e = Entity::new(
            &self.rel_path,
            self.abs_path.clone(),
            &name,
            SymbolKind::Method,
            node.start_position().row + 1,
            node.end_position().row + 1,
            node.start_byte(),
            node.end_byte(),
            false,
            Some(class_id.to_string()),
        );
        self.symbol_stack.push(e.id.clone());
        self.entities.push(e);
        self.record_param_types(node);
        // Walk method body for refs (including this.* refs)
        self.visit_children(node, false);
        self.symbol_stack.pop();
    }

    fn visit_class_field(&mut self, node: &Node, class_id: &str) {
        // `foo = () => {}` field holding arrow function -> treat as method-like entity.
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        let mut name: Option<String> = None;
        let mut value_kind: Option<&str> = None;
        let mut value_node: Option<Node> = None;
        for c in &children {
            match c.kind() {
                "property_identifier" | "identifier" | "private_property_identifier" => {
                    if name.is_none() {
                        name = Some(self.text(c).to_string());
                    }
                }
                "arrow_function" | "function_expression" => {
                    value_kind = Some(c.kind());
                    value_node = Some(*c);
                }
                _ => {}
            }
        }
        if let (Some(n), Some(vk)) = (name, value_kind) {
            let kind = if vk == "arrow_function" {
                SymbolKind::ArrowFunction
            } else {
                SymbolKind::FunctionExpression
            };
            let e = Entity::new(
                &self.rel_path,
                self.abs_path.clone(),
                &n,
                kind,
                node.start_position().row + 1,
                node.end_position().row + 1,
                node.start_byte(),
                node.end_byte(),
                false,
                Some(class_id.to_string()),
            );
            self.symbol_stack.push(e.id.clone());
            self.entities.push(e);
            if let Some(v) = value_node {
                self.record_param_types(&v);
                self.visit_children(&v, false);
            }
            // Field type annotations reference types (`header: SetHeaders = ...`).
            for c in &children {
                if c.kind() == "type_annotation" {
                    self.visit_typed_inner(c);
                }
            }
            self.symbol_stack.pop();
        } else {
            // plain field -> variable-like? record refs inside.
            self.visit_children(node, false);
        }
    }

    fn visit_lexical(&mut self, node: &Node, exported: bool) {
        // `for (let i = ...)` / `for (const x of ...)` declarators are block-scoped;
        // without full block-scope tracking we cannot prove them unused — skip entity
        // creation (their uses are still recorded as references).
        let in_for_init = node
            .parent()
            .map(|p| {
                matches!(
                    p.kind(),
                    "for_statement" | "for_in_statement" | "for_of_statement"
                )
            })
            .unwrap_or(false);
        if in_for_init {
            self.visit_children(node, false);
            return;
        }
        let mut cursor = node.walk();
        let declarators: Vec<Node> = node
            .children(&mut cursor)
            .filter(|c| c.kind() == "variable_declarator")
            .collect();
        let single = declarators.len() == 1;
        for decl in declarators {
            let name_node = decl.child_by_field_name("name");
            let value_node = decl.child_by_field_name("value");
            // CommonJS: `const lib = require("./lib")` / `const {a, b: c} = require("./lib")`.
            // These introduce import bindings (like ESM imports), not removable variables.
            if let Some(v) = value_node {
                if let Some(spec) = require_call_spec(self.source, &v) {
                    // The callee is still a usage (keeps createRequire variables alive).
                    self.push_ref(&v, "require", None, RefKind::Value, RefContext::Identifier);
                    self.emit_require_binding(&decl, &name_node, &spec);
                    continue;
                }
            }
            let Some(name_node) = name_node else {
                // destructuring — visit for refs, no entity
                self.visit_children(&decl, false);
                continue;
            };
            // Skip destructuring patterns (object_pattern etc.)
            if name_node.kind() != "identifier" {
                // `const {a} = expr` (non-require): visit for refs; record annotated types below.
                self.visit_children(&decl, false);
                continue;
            }
            let name = self.text(&name_node).to_string();
            // `export const y = new X()` publishes an instance outward.
            if exported {
                if let Some(v) = value_node {
                    if v.kind() == "new_expression" {
                        if let Some(ctor) = v.child_by_field_name("constructor") {
                            let ctxt = self.text(&ctor).to_string();
                            let cls = ctxt.split('.').next().unwrap_or(&ctxt).trim().to_string();
                            if !cls.is_empty() {
                                self.escaped_classes.push(cls);
                            }
                        }
                    }
                }
            }
            // Track `const s = new Service()` and `const s: Service = ...` for member-call resolution.
            if let Some(v) = value_node {
                if v.kind() == "new_expression" {
                    if let Some(ctor) = v.child_by_field_name("constructor") {
                        let ctxt = self.text(&ctor).to_string();
                        let cls = ctxt.split('.').next().unwrap_or(&ctxt).trim().to_string();
                        if !cls.is_empty()
                            && cls
                                .chars()
                                .next()
                                .map(|c| c.is_uppercase())
                                .unwrap_or(false)
                        {
                            self.var_types.push((name.clone(), cls));
                        }
                    }
                }
            }
            if let Some(ann) = decl.child_by_field_name("type") {
                if let Some(cls) = first_type_identifier(self.source, &ann) {
                    self.var_types.push((name.clone(), cls));
                }
                // Type annotations reference types (`const p: PluginFunc<Opts>`).
                self.visit_typed_inner(&ann);
            }
            let (kind, is_fn) = match value_node.map(|v| v.kind().to_string()) {
                Some(k) if k == "arrow_function" => (SymbolKind::ArrowFunction, true),
                Some(k) if k == "function_expression" || k == "function" => {
                    (SymbolKind::FunctionExpression, true)
                }
                Some(k) if k == "class" || k == "class_expression" => {
                    // `const Foo = class {}` -> class entity
                    let (s, e) = if single {
                        (node.start_byte(), node.end_byte())
                    } else {
                        (decl.start_byte(), decl.end_byte())
                    };
                    let (sl, el) = if single {
                        (node.start_position().row + 1, node.end_position().row + 1)
                    } else {
                        (decl.start_position().row + 1, decl.end_position().row + 1)
                    };
                    let en = Entity::new(
                        &self.rel_path,
                        self.abs_path.clone(),
                        &name,
                        SymbolKind::Class,
                        sl,
                        el,
                        s,
                        e,
                        exported,
                        None,
                    );
                    self.symbol_stack.push(en.id.clone());
                    let en_id = en.id.clone();
                    self.entities.push(en);
                    if let Some(v) = value_node {
                        // Named class expressions can extend (override-aware).
                        self.record_class_heritage(&v, &en_id);
                        self.visit_children(&v, false);
                    }
                    self.symbol_stack.pop();
                    continue;
                }
                _ => (SymbolKind::Variable, false),
            };
            let (s, e) = if single {
                (node.start_byte(), node.end_byte())
            } else {
                (decl.start_byte(), decl.end_byte())
            };
            let (sl, el) = if single {
                (node.start_position().row + 1, node.end_position().row + 1)
            } else {
                (decl.start_position().row + 1, decl.end_position().row + 1)
            };
            let en = Entity::new(
                &self.rel_path,
                self.abs_path.clone(),
                &name,
                kind,
                sl,
                el,
                s,
                e,
                exported,
                self.symbol_stack.last().cloned(),
            );
            self.symbol_stack.push(en.id.clone());
            self.entities.push(en);
            if is_fn {
                if let Some(v) = value_node {
                    self.record_param_types(&v);
                    self.visit_children(&v, false);
                }
            } else if let Some(v) = value_node {
                // Initializer may contain references: `const x = foo`.
                self.visit(&v, false);
            }
            self.symbol_stack.pop();
        }
    }

    /// Emit import bindings for `require("./x")` declarators.
    fn emit_require_binding(&mut self, decl: &Node, name_node: &Option<Node>, spec: &str) {
        let line = self.line(decl);
        let Some(nn) = name_node else {
            // `const {} = require(...)` — nothing bound.
            return;
        };
        if nn.kind() == "identifier" {
            let local = self.text(nn).to_string();
            // `const lib = require("./lib")` binds the whole module (namespace-like).
            self.imports.push(ImportRec {
                from_file: self.rel_path.clone(),
                source_raw: spec.to_string(),
                resolved_file: None,
                local_name: local,
                original_name: "*".to_string(),
                is_type_only: false,
                kind: ImportKind::Require,
                line,
            });
            return;
        }
        // `const {a, b: c} = require("./lib")` — named bindings.
        for (orig, local) in destructure_pairs(self.source, nn) {
            self.imports.push(ImportRec {
                from_file: self.rel_path.clone(),
                source_raw: spec.to_string(),
                resolved_file: None,
                local_name: local,
                original_name: orig,
                is_type_only: false,
                kind: ImportKind::Require,
                line,
            });
        }
    }

    /// `return new X()` / `return <X-typed var>`: the instance may escape.
    /// `return { ...methods... }`: the returned visitor/hook object escapes to the caller.
    fn visit_return(&mut self, node: &Node) {
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for c in &children {
            match c.kind() {
                "new_expression" => {
                    if let Some(ctor) = c.child_by_field_name("constructor") {
                        let ctxt = self.text(&ctor).to_string();
                        let cls = ctxt.split('.').next().unwrap_or(&ctxt).trim().to_string();
                        if !cls.is_empty() {
                            self.escaped_classes.push(cls);
                        }
                    }
                }
                "identifier" => {
                    let n = self.text(c).to_string();
                    self.returned_idents.push(n);
                }
                "object" => {
                    self.collect_surface_methods(c);
                }
                "array" => {
                    // `return [{ fix() {} }, ...]`: visitor/hook objects.
                    let mut cursor = c.walk();
                    let elems: Vec<Node> = c.children(&mut cursor).collect();
                    for el in &elems {
                        if el.kind() == "object" {
                            self.collect_surface_methods(el);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    /// Record methods of object literals passed as call arguments, at any depth
    /// (`report({ suggest: [{ fix() {} }] })`, `register(class extends B {
    /// handle() {} })`): methods created inside call arguments are invoked as
    /// callbacks/hooks/visitors by the callee.
    fn record_hook_methods(&mut self, args_node: &Node) {
        let mut stack: Vec<Node> = {
            let mut cursor = args_node.walk();
            args_node.children(&mut cursor).collect()
        };
        while let Some(n) = stack.pop() {
            if n.kind() == "method_definition" {
                if let Some(name_node) = n.child_by_field_name("name") {
                    let name = strip_quotes(self.text(&name_node));
                    self.hook_methods.push((name, n.start_position().row + 1));
                }
            }
            let mut cursor = n.walk();
            let children: Vec<Node> = n.children(&mut cursor).collect();
            for c in children {
                stack.push(c);
            }
        }
        // Also record direct object-arg methods as surface for the reason text.
        let mut cursor = args_node.walk();
        let children: Vec<Node> = args_node.children(&mut cursor).collect();
        for c in &children {
            if c.kind() == "object" {
                self.collect_surface_methods(c);
            }
        }
    }

    /// Methods of an object literal that forms a module surface
    /// (`module.exports = {...}`, `export default {...}`, or hook args).
    fn collect_surface_methods(&mut self, obj_node: &Node) {
        let mut cursor = obj_node.walk();
        let members: Vec<Node> = obj_node.children(&mut cursor).collect();
        for m in &members {
            if m.kind() == "method_definition" {
                if let Some(name_node) = m.child_by_field_name("name") {
                    let n = self.text(&name_node).to_string();
                    self.surface_methods.push((n, m.start_position().row + 1));
                }
            }
        }
    }

    /// `handlers[key](...)`: computed dispatch — any method of the base may run.
    /// `obj["literal"]()` is statically known: record it as a member reference.
    fn record_dispatch_base(&mut self, call_node: &Node, _args_node: &Node) {
        let Some(func) = call_node.child_by_field_name("function") else {
            return;
        };
        if func.kind() != "subscript_expression" {
            return;
        }
        let (obj, is_literal, lit_name) = subscript_parts(self.source, &func);
        let Some(o) = obj else {
            return;
        };
        if is_literal {
            if let Some(n) = lit_name {
                self.push_ref(call_node, &n, Some(o), RefKind::Value, RefContext::Member);
            }
            return;
        }
        let base_root = o.split('.').next().unwrap_or(&o).trim().to_string();
        if !base_root.is_empty()
            && base_root != "this"
            && base_root != "exports"
            && base_root != "module"
        {
            self.dynamic_dispatch_bases
                .push((base_root, self.current_symbol()));
        }
    }

    /// Record `(param: Type)` annotations as variable types so `param.method()`
    /// receivers resolve (`function f(c: Cursor) { c.moveNext(); }`).
    fn record_param_types(&mut self, node: &Node) {
        let Some(params) = node.child_by_field_name("parameters") else {
            return;
        };
        let mut cursor = params.walk();
        let ps: Vec<Node> = params.children(&mut cursor).collect();
        for p in &ps {
            if !matches!(
                p.kind(),
                "required_parameter" | "optional_parameter" | "rest_pattern"
            ) {
                continue;
            }
            let name = p
                .child_by_field_name("pattern")
                .filter(|n| n.kind() == "identifier")
                .map(|n| self.text(&n).to_string());
            let ty = p
                .child_by_field_name("type")
                .and_then(|t| first_type_identifier(self.source, &t));
            if let (Some(n), Some(t)) = (name, ty) {
                if n != "this" {
                    self.var_types.push((n, t));
                }
            }
        }
    }

    /// Record a repo-relative directory prefix that may be loaded dynamically.
    fn record_dynamic_prefix(&mut self, raw_prefix: &str) {
        let importer_dir = match self.rel_path.rfind('/') {
            Some(i) => &self.rel_path[..i],
            None => "",
        };
        let joined = if importer_dir.is_empty() {
            raw_prefix.to_string()
        } else {
            format!("{}/{}", importer_dir, raw_prefix)
        };
        let norm = normalize_rel_slash(&joined);
        if norm.is_empty() {
            return;
        }
        // `./plugins/` protects the dir itself; `./plug` protects its parent.
        let dir = if raw_prefix.ends_with('/') {
            norm
        } else {
            match norm.rfind('/') {
                Some(i) => norm[..i].to_string(),
                None => String::new(),
            }
        };
        if !dir.is_empty() && !self.dynamic_prefixes.contains(&dir) {
            self.dynamic_prefixes.push(dir);
        }
    }

    /// Handle CommonJS export assignments:
    /// `exports.foo = ...`, `module.exports.foo = ...`, `module.exports = {...}`.
    /// Returns true when RHS children were visited internally (caller must skip them).
    fn visit_assignment(&mut self, node: &Node) -> bool {
        let left = node.child_by_field_name("left");
        let right = node.child_by_field_name("right");
        let (Some(l), Some(r)) = (left, right) else {
            return false;
        };
        let ltext = self.text(&l).to_string();
        let line = self.line(node);
        // `obj.prop = ...` / `obj[prop] = ...` publishes a property outward
        // (plugin/prototype registration). Excludes `this.*` (construction),
        // `exports.*` and `module.exports*` (export marking, handled below).
        if matches!(l.kind(), "member_expression" | "subscript_expression") {
            let base = l
                .child_by_field_name("object")
                .and_then(|o| o.utf8_text(self.source.as_bytes()).ok())
                .unwrap_or("");
            let base_root = base.split('.').next().unwrap_or(base).trim();
            if base_root != "this"
                && base_root != "exports"
                && base_root != "module"
                && !base_root.is_empty()
            {
                self.publishes_members = true;
            }
        }
        // `exports.foo = RHS` / `module.exports.foo = RHS`
        for prefix in ["exports.", "module.exports."] {
            if let Some(name) = ltext.strip_prefix(prefix) {
                let name = name.trim().to_string();
                if name.is_empty() || name.contains('.') || name.contains('[') {
                    return false;
                }
                let rk = r.kind();
                if matches!(rk, "function_expression" | "function" | "arrow_function") {
                    let ekind = if rk == "arrow_function" {
                        SymbolKind::ArrowFunction
                    } else {
                        SymbolKind::FunctionExpression
                    };
                    let e = Entity::new(
                        &self.rel_path,
                        self.abs_path.clone(),
                        &name,
                        ekind,
                        node.start_position().row + 1,
                        node.end_position().row + 1,
                        node.start_byte(),
                        node.end_byte(),
                        true,
                        None,
                    );
                    self.symbol_stack.push(e.id.clone());
                    self.entities.push(e);
                    self.visit_children(&r, false);
                    self.symbol_stack.pop();
                } else if matches!(rk, "class" | "class_expression") {
                    let e = Entity::new(
                        &self.rel_path,
                        self.abs_path.clone(),
                        &name,
                        SymbolKind::Class,
                        node.start_position().row + 1,
                        node.end_position().row + 1,
                        node.start_byte(),
                        node.end_byte(),
                        true,
                        None,
                    );
                    let eid = e.id.clone();
                    self.entities.push(e);
                    // Named class expressions can still extend (override-aware).
                    self.record_class_heritage(&r, &eid);
                    self.visit_children(&r, false);
                } else {
                    // `exports.foo = <expr>` — mark same-file `foo` exported if present.
                    self.local_exports.push(LocalExport {
                        local_name: name.clone(),
                        exported_name: name,
                        is_default: false,
                        is_type_only: false,
                        line,
                    });
                    self.visit(&r, false);
                }
                return true;
            }
        }
        // `module.exports = RHS`
        if ltext == "module.exports" {
            let rk = r.kind();
            if rk == "object" || rk == "object_pattern" {
                for (orig, local) in object_literal_exports(self.source, &r) {
                    self.local_exports.push(LocalExport {
                        local_name: local,
                        exported_name: orig,
                        is_default: false,
                        is_type_only: false,
                        line,
                    });
                }
                // Methods of the exported surface object are public API.
                self.collect_surface_methods(&r);
            } else if rk == "identifier" {
                let n = self.text(&r).to_string();
                self.local_exports.push(LocalExport {
                    local_name: n,
                    exported_name: "default".to_string(),
                    is_default: true,
                    is_type_only: false,
                    line,
                });
            } else if matches!(rk, "class" | "class_expression") {
                // `module.exports = class X extends Y { ... }`: the whole module.
                let name = r
                    .child_by_field_name("name")
                    .map(|n| self.text(&n).to_string())
                    .unwrap_or_else(|| "default".to_string());
                let mut e = Entity::new(
                    &self.rel_path,
                    self.abs_path.clone(),
                    &name,
                    SymbolKind::Class,
                    node.start_position().row + 1,
                    node.end_position().row + 1,
                    node.start_byte(),
                    node.end_byte(),
                    true,
                    None,
                );
                e.is_default_export = true;
                let eid = e.id.clone();
                self.symbol_stack.push(eid.clone());
                self.entities.push(e);
                self.record_class_heritage(&r, &eid);
                self.visit_children(&r, false);
                self.symbol_stack.pop();
                return true;
            } else if matches!(rk, "function" | "function_expression" | "arrow_function") {
                // `module.exports = function...` / `=> ...`: the whole module.
                let ekind = if rk == "arrow_function" {
                    SymbolKind::ArrowFunction
                } else {
                    SymbolKind::FunctionExpression
                };
                let mut e = Entity::new(
                    &self.rel_path,
                    self.abs_path.clone(),
                    "default",
                    ekind,
                    node.start_position().row + 1,
                    node.end_position().row + 1,
                    node.start_byte(),
                    node.end_byte(),
                    true,
                    None,
                );
                e.is_default_export = true;
                self.symbol_stack.push(e.id.clone());
                self.entities.push(e);
                self.record_param_types(&r);
                self.visit_children(&r, false);
                self.symbol_stack.pop();
                return true;
            }
        }
        false
    }

    // ---- imports / exports ----

    fn visit_import_statement(&mut self, node: &Node) {
        let src = child_string_literal(self.source, node, "source");
        let line = self.line(node);
        let is_type_only =
            node_text_contains(self.source, node, "import type") || has_type_modifier(node);
        let Some(source_raw) = src else {
            return;
        };
        // Find import clause
        let mut found_any = false;
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for child in &children {
            if child.kind() == "import_clause" {
                let mut c2 = child.walk();
                let parts: Vec<Node> = child.children(&mut c2).collect();
                for p in &parts {
                    match p.kind() {
                        "identifier" => {
                            // default import: `import foo from`
                            let local = self.text(p).to_string();
                            self.imports.push(ImportRec {
                                from_file: self.rel_path.clone(),
                                source_raw: source_raw.clone(),
                                resolved_file: None,
                                local_name: local,
                                original_name: "default".to_string(),
                                is_type_only,
                                kind: ImportKind::Default,
                                line,
                            });
                            found_any = true;
                        }
                        "namespace_import" => {
                            // `* as ns`
                            let ns = last_identifier_text(self.source, p)
                                .unwrap_or("ns".to_string());
                            self.imports.push(ImportRec {
                                from_file: self.rel_path.clone(),
                                source_raw: source_raw.clone(),
                                resolved_file: None,
                                local_name: ns,
                                original_name: "*".to_string(),
                                is_type_only,
                                kind: ImportKind::Namespace,
                                line,
                            });
                            found_any = true;
                        }
                        "named_imports" => {
                            let mut c3 = p.walk();
                            let specs: Vec<Node> = p.children(&mut c3).collect();
                            for s in &specs {
                                if s.kind() == "import_specifier" {
                                    let (orig, local) = import_specifier_names(self.source, s);
                                    self.imports.push(ImportRec {
                                        from_file: self.rel_path.clone(),
                                        source_raw: source_raw.clone(),
                                        resolved_file: None,
                                        local_name: local,
                                        original_name: orig,
                                        is_type_only: is_type_only || spec_is_type(s),
                                        kind: ImportKind::Named,
                                        line,
                                    });
                                    found_any = true;
                                }
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
        if !found_any {
            // side-effect import: `import "./foo"`
            self.imports.push(ImportRec {
                from_file: self.rel_path.clone(),
                source_raw: source_raw.clone(),
                resolved_file: None,
                local_name: "*side-effect*".to_string(),
                original_name: "*".to_string(),
                is_type_only,
                kind: ImportKind::SideEffect,
                line,
            });
        }
        // Imported names create TYPE or VALUE edges from this file; record a file-level ref for graph.
        // Symbol-level edges are resolved later via resolver.
    }

    fn visit_export_statement(&mut self, node: &Node) {
        let line = self.line(node);
        let source_raw = child_string_literal(self.source, node, "source");
        let is_type_only =
            node_text_contains(self.source, node, "export type") || has_type_modifier(node);

        // `export = foo;` (TS export-assignment; parses as export_statement
        // holding `=` in some grammars): `foo` is the module export.
        if source_raw.is_none() && node_text_contains(self.source, node, "export =") {
            let mut cursor = node.walk();
            let children: Vec<Node> = node.children(&mut cursor).collect();
            for child in &children {
                if child.kind() == "identifier" {
                    let n = self.text(child).to_string();
                    self.local_exports.push(LocalExport {
                        local_name: n,
                        exported_name: "default".to_string(),
                        is_default: true,
                        is_type_only,
                        line,
                    });
                } else if child.kind() != "export" && child.kind() != "=" && child.kind() != ";" {
                    self.visit(child, false);
                }
            }
            return;
        }
        if let Some(src) = source_raw {
            // Re-export: `export { a as b } from "./x"`, `export * from`, `export type {...} from`
            let mut saw_wildcard = false;
            let mut specs: Vec<(String, String)> = Vec::new();
            let mut cursor = node.walk();
            let children: Vec<Node> = node.children(&mut cursor).collect();
            for child in &children {
                match child.kind() {
                    "namespace_export" | "export_all" | "*" => {
                        saw_wildcard = true;
                    }
                    "export_clause" | "named_exports" => {
                        let mut c2 = child.walk();
                        let ss: Vec<Node> = child.children(&mut c2).collect();
                        for s in &ss {
                            if s.kind() == "export_specifier" {
                                let (orig, exported) = export_specifier_names(self.source, s);
                                specs.push((orig, exported));
                            }
                        }
                    }
                    "export_specifier" => {
                        let (orig, exported) = export_specifier_names(self.source, child);
                        specs.push((orig, exported));
                    }
                    _ => {
                        if self.text(child) == "*" {
                            saw_wildcard = true;
                        }
                    }
                }
            }
            // Fallback: detect `*` textually
            if specs.is_empty() && node_text_contains(self.source, node, "export *") {
                saw_wildcard = true;
            }
            if saw_wildcard {
                // `export * as ns from` vs `export * from`
                let exported_name = if node_text_contains(self.source, node, "export * as") {
                    extract_star_as_name(self.source, node).unwrap_or("*".to_string())
                } else {
                    "*".to_string()
                };
                self.reexports.push(ReExportRec {
                    from_file: self.rel_path.clone(),
                    source_raw: src.clone(),
                    resolved_file: None,
                    original_name: "*".to_string(),
                    exported_name,
                    is_type_only,
                    is_wildcard: true,
                    line,
                });
            }
            for (orig, exported) in &specs {
                self.reexports.push(ReExportRec {
                    from_file: self.rel_path.clone(),
                    source_raw: src.clone(),
                    resolved_file: None,
                    original_name: orig.clone(),
                    exported_name: exported.clone(),
                    is_type_only,
                    is_wildcard: false,
                    line,
                });
            }
            if specs.is_empty() && !saw_wildcard {
                // `export * from` missed by grammar walk — still record wildcard.
                self.reexports.push(ReExportRec {
                    from_file: self.rel_path.clone(),
                    source_raw: src.clone(),
                    resolved_file: None,
                    original_name: "*".to_string(),
                    exported_name: "*".to_string(),
                    is_type_only,
                    is_wildcard: true,
                    line,
                });
            }
            return;
        }

        // No source: either `export <declaration>` or `export { local }` or `export default ...`.
        let node_text = self.text(node).to_string();
        let trimmed = node_text.trim_start_matches("export").trim_start();
        let is_default = trimmed.starts_with("default");
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        let mut saw_declaration = false;
        for child in &children {
            match child.kind() {
                "export_clause" | "named_exports" => {
                    let mut c2 = child.walk();
                    let ss: Vec<Node> = child.children(&mut c2).collect();
                    for s in &ss {
                        if s.kind() == "export_specifier" {
                            let (orig, exported) = export_specifier_names(self.source, s);
                            self.local_exports.push(LocalExport {
                                local_name: orig,
                                exported_name: exported.clone(),
                                is_default: exported == "default",
                                is_type_only,
                                line,
                            });
                        }
                    }
                }
                "export_specifier" => {
                    let (orig, exported) = export_specifier_names(self.source, child);
                    self.local_exports.push(LocalExport {
                        local_name: orig,
                        exported_name: exported.clone(),
                        is_default: exported == "default",
                        is_type_only,
                        line,
                    });
                }
                "function_declaration"
                | "generator_function_declaration"
                | "class_declaration"
                | "abstract_class_declaration"
                | "lexical_declaration"
                | "variable_declaration"
                | "interface_declaration"
                | "type_alias_declaration"
                | "enum_declaration"
                | "internal_module"
                | "module"
                | "namespace_declaration" => {
                    saw_declaration = true;
                    self.visit(child, true);
                }
                "identifier" => {
                    // `export default foo;`
                    if is_default {
                        let n = self.text(child).to_string();
                        self.local_exports.push(LocalExport {
                            local_name: n.clone(),
                            exported_name: "default".to_string(),
                            is_default: true,
                            is_type_only,
                            line,
                        });
                    }
                }
                "comment" | "decorator" | "export" | "default" | "type" | "{" | "}" | "," | ";" => {
                }
                "object" => {
                    // `export default { create() {} }`: surface object methods are public API.
                    if is_default {
                        self.collect_surface_methods(child);
                    }
                    self.visit(child, false);
                    saw_declaration = true;
                }
                _ => {
                    if is_default && child.kind().contains("declaration") {
                        saw_declaration = true;
                        self.visit(child, true);
                    } else if !saw_declaration {
                        // Recurse for e.g. `export default function...` anonymous
                        let k = child.kind();
                        if k == "function"
                            || k == "function_expression"
                            || k == "arrow_function"
                            || k == "class"
                            || k == "call_expression"
                        {
                            // anonymous default export expression — synthesize entity
                            if k == "function"
                                || k == "function_expression"
                                || k == "arrow_function"
                            {
                                let ekind = if k == "arrow_function" {
                                    SymbolKind::ArrowFunction
                                } else {
                                    SymbolKind::Function
                                };
                                let mut e = Entity::new(
                                    &self.rel_path,
                                    self.abs_path.clone(),
                                    "default",
                                    ekind,
                                    child.start_position().row + 1,
                                    child.end_position().row + 1,
                                    child.start_byte(),
                                    child.end_byte(),
                                    true,
                                    None,
                                );
                                e.is_default_export = true;
                                self.symbol_stack.push(e.id.clone());
                                self.entities.push(e);
                                self.visit_children(child, false);
                                self.symbol_stack.pop();
                            } else {
                                self.visit(child, false);
                            }
                            saw_declaration = true;
                        } else if k != "string" && k != "string_fragment" {
                            self.visit(child, true);
                            saw_declaration = true;
                        }
                    }
                }
            }
        }
        if !saw_declaration && is_default && self.local_exports.is_empty() {
            // `export default <expr>` — record nothing more; default-exported file counts as exported surface.
        }
    }

    // ---- references ----

    fn visit_call(&mut self, node: &Node) {
        // Calls outside any declaration are top-level side effects.
        if self.symbol_stack.is_empty() {
            self.has_top_level_calls = true;
        }
        // Object literals passed as arguments whose methods are callbacks/hooks
        // invoked by the callee (`new Readable({ read() {} })`).
        if let Some(args) = node.child_by_field_name("arguments") {
            self.record_hook_methods(&args);
            self.record_dispatch_base(node, &args);
        }
        // `path.join(__dirname, "static", name)` / `path.resolve(...)`: even
        // outside require()/import(), this computes a dynamically-loaded
        // directory — protect the static prefix (formatters, rules, plugins).
        if let Some(func) = node.child_by_field_name("function") {
            if func.kind() == "member_expression" {
                let (_, prop) = member_parts(self.source, &func);
                if matches!(prop.as_deref(), Some("join") | Some("resolve")) {
                    if let Some(pre) = path_join_prefix(self.source, node) {
                        self.record_dynamic_prefix(&pre);
                    }
                }
            }
        }
        let func = node.child_by_field_name("function");
        let args = node.child_by_field_name("arguments");
        if let Some(f) = func {
            // `import("...")` may parse as a call whose function is the `import` keyword.
            if f.kind() == "import" {
                if let Some(a) = args {
                    if let Some(lit) = first_string_arg(self.source, &a) {
                        self.imports.push(ImportRec {
                            from_file: self.rel_path.clone(),
                            source_raw: lit,
                            resolved_file: None,
                            local_name: "*dynamic*".to_string(),
                            original_name: "*".to_string(),
                            is_type_only: false,
                            kind: ImportKind::DynamicLiteral,
                            line: self.line(node),
                        });
                    } else {
                        if let Some(pre) = dynamic_prefix_of_arg(self.source, &a) {
                            self.record_dynamic_prefix(&pre);
                        }
                        self.dynamic_details.push(format!(
                            "{}:{}: dynamic import() with non-literal argument",
                            self.rel_path,
                            self.line(node)
                        ));
                    }
                }
                return;
            }
            // `import.meta.resolve(expr)` loads modules dynamically, like import().
            // A literal is a resolvable edge; anything else is dynamic risk.
            if f.kind() == "member_expression" {
                let (obj, prop) = member_parts(self.source, &f);
                if let (Some(o), Some(p)) = (obj, prop) {
                    if p == "resolve" && o.ends_with("import.meta") {
                        if let Some(a) = args {
                            if let Some(lit) = first_string_arg(self.source, &a) {
                                self.imports.push(ImportRec {
                                    from_file: self.rel_path.clone(),
                                    source_raw: lit,
                                    resolved_file: None,
                                    local_name: "*dynamic*".to_string(),
                                    original_name: "*".to_string(),
                                    is_type_only: false,
                                    kind: ImportKind::DynamicLiteral,
                                    line: self.line(node),
                                });
                            } else {
                                if let Some(pre) = dynamic_prefix_of_arg(self.source, &a) {
                                    self.record_dynamic_prefix(&pre);
                                }
                                self.dynamic_details.push(format!(
                                    "{}:{}: dynamic import.meta.resolve() with non-literal argument",
                                    self.rel_path,
                                    self.line(node)
                                ));
                            }
                        }
                        return;
                    }
                }
            }
        }
        if let Some(f) = func {
            let ftext = self.text(&f).to_string();
            match f.kind() {
                "identifier" => {
                    if ftext == "require" {
                        if let Some(a) = args {
                            // The callee itself is a usage (keeps `createRequire`
                            // variables alive: `const require = createRequire(...); require("x")`).
                            self.push_ref(
                                node,
                                "require",
                                None,
                                RefKind::Value,
                                RefContext::Identifier,
                            );
                            if let Some(lit) = first_string_arg(self.source, &a)
                                .or_else(|| eval_path_call(self.source, &a).flatten())
                            {
                                self.imports.push(ImportRec {
                                    from_file: self.rel_path.clone(),
                                    source_raw: lit,
                                    resolved_file: None,
                                    local_name: "*require*".to_string(),
                                    original_name: "*".to_string(),
                                    is_type_only: false,
                                    kind: ImportKind::Require,
                                    line: self.line(node),
                                });
                            } else {
                                if let Some(pre) = dynamic_prefix_of_arg(self.source, &a) {
                                    self.record_dynamic_prefix(&pre);
                                }
                                self.dynamic_details.push(format!(
                                    "{}:{}: dynamic require() with non-literal argument",
                                    self.rel_path,
                                    self.line(node)
                                ));
                            }
                        }
                        return;
                    } else if ftext == "eval" || ftext == "Function" {
                        self.dynamic_details.push(format!(
                            "{}:{}: direct {}() usage",
                            self.rel_path,
                            self.line(node),
                            ftext
                        ));
                        return;
                    } else if ftext == "import" {
                        // `import("...")` parsed as call in some grammars
                        if let Some(a) = args {
                            if let Some(lit) = first_string_arg(self.source, &a) {
                                self.imports.push(ImportRec {
                                    from_file: self.rel_path.clone(),
                                    source_raw: lit,
                                    resolved_file: None,
                                    local_name: "*dynamic*".to_string(),
                                    original_name: "*".to_string(),
                                    is_type_only: false,
                                    kind: ImportKind::DynamicLiteral,
                                    line: self.line(node),
                                });
                            } else {
                                if let Some(a2) = args {
                                    if let Some(pre) = dynamic_prefix_of_arg(self.source, &a2) {
                                        self.record_dynamic_prefix(&pre);
                                    }
                                }
                                self.dynamic_details.push(format!(
                                    "{}:{}: dynamic import() with non-literal argument",
                                    self.rel_path,
                                    self.line(node)
                                ));
                            }
                        }
                        return;
                    }
                    self.push_ref(node, &ftext, None, RefKind::Value, RefContext::Call);
                }
                "member_expression" => {
                    // `obj.method()` — record method name with base for safe resolution.
                    // (`exports.*`/`module.*` are export markings, not uses.)
                    let (obj, prop) = member_parts(self.source, &f);
                    let is_marking = obj
                        .as_deref()
                        .map(|o| {
                            let r = o.split('.').next().unwrap_or(o);
                            r == "exports" || r == "module"
                        })
                        .unwrap_or(false);
                    if !is_marking {
                        if let Some(method) = prop {
                            if method == "eval" {
                                self.dynamic_details.push(format!(
                                    "{}:{}: member eval() usage",
                                    self.rel_path,
                                    self.line(node)
                                ));
                            }
                            self.push_ref(node, &method, obj, RefKind::Value, RefContext::Member);
                        }
                    }
                }
                _ => {}
            }
        }
    }

    fn visit_new(&mut self, node: &Node) {
        if self.symbol_stack.is_empty() {
            self.has_top_level_calls = true;
        }
        if let Some(args) = node.child_by_field_name("arguments") {
            self.record_hook_methods(&args);
        }
        // `new (class { onEvent() {} })`: methods of an inline class passed as
        // the constructor are hooks invoked through the instance. Walk the
        // whole constructor subtree (it may be parenthesized).
        if let Some(ctor) = node.child_by_field_name("constructor") {
            let mut stack: Vec<Node> = {
                let mut cursor = ctor.walk();
                ctor.children(&mut cursor).collect()
            };
            while let Some(n) = stack.pop() {
                if n.kind() == "method_definition" {
                    if let Some(name_node) = n.child_by_field_name("name") {
                        let name = strip_quotes(self.text(&name_node));
                        self.hook_methods.push((name, n.start_position().row + 1));
                    }
                }
                let mut cursor = n.walk();
                let children: Vec<Node> = n.children(&mut cursor).collect();
                for c in children {
                    stack.push(c);
                }
            }
        }
        if let Some(ctor) = node.child_by_field_name("constructor") {
            let t = self.text(&ctor).to_string();
            // ctor may be `Foo` or `ns.Foo`
            let name = t.split('.').next_back().unwrap_or(&t).trim().to_string();
            let base = if t.contains('.') {
                t.split('.').next().map(|s| s.to_string())
            } else {
                None
            };
            self.push_ref(node, &name, base, RefKind::Value, RefContext::New);
        }
    }

    fn visit_jsx_open(&mut self, node: &Node) {
        // children include identifiers / nested_identifier
        let mut cursor = node.walk();
        let children: Vec<Node> = node.children(&mut cursor).collect();
        for c in &children {
            match c.kind() {
                "identifier" => {
                    let n = self.text(c).to_string();
                    // lowercase = intrinsic element (<div>), skip
                    if n.chars()
                        .next()
                        .map(|ch| ch.is_uppercase())
                        .unwrap_or(false)
                    {
                        self.push_ref(node, &n, None, RefKind::Value, RefContext::Jsx);
                    }
                }
                "nested_identifier" | "jsx_namespace_name" | "jsx_nested_identifier" => {
                    let full = self.text(c).to_string();
                    let first = full
                        .split(['.', ':'])
                        .next()
                        .unwrap_or(&full);
                    if first
                        .chars()
                        .next()
                        .map(|ch| ch.is_uppercase())
                        .unwrap_or(false)
                    {
                        self.push_ref(node, first, None, RefKind::Value, RefContext::Jsx);
                    }
                }
                _ => {}
            }
        }
    }

    fn visit_member(&mut self, node: &Node) {
        // Skip when this member expression is the callee of a call (`s.used()`):
        // visit_call already recorded it — avoids double-counted callers.
        if let Some(parent) = node.parent() {
            if parent.kind() == "call_expression" {
                if let Some(f) = parent.child_by_field_name("function") {
                    if f.id() == node.id() {
                        return;
                    }
                }
            }
        }
        let (obj, prop) = member_parts(self.source, node);
        if let (Some(o), Some(p)) = (obj, prop) {
            // `exports.foo` / `module.exports` are export markings, not uses.
            let o_root = o.split('.').next().unwrap_or(&o);
            if o_root == "exports" || o_root == "module" {
                return;
            }
            if o == "this" {
                self.push_ref(
                    node,
                    &p,
                    Some("this".to_string()),
                    RefKind::Value,
                    RefContext::ThisMethod,
                );
            } else {
                // Bare `obj.prop` (property value, condition, ...) may reference a
                // method owned by `obj` (object literal, namespace, class). Record it;
                // resolution links only when the receiver is provably safe.
                self.push_ref(node, &p, Some(o), RefKind::Value, RefContext::Member);
            }
        }
    }

    fn visit_import_expr(&mut self, node: &Node) {
        // `import("...")` as import_expression with `source` field or first string child.
        let mut found_literal: Option<String> = None;
        if let Some(src) = node.child_by_field_name("source") {
            if let Some(lit) = string_literal_value(self.source, &src) {
                found_literal = Some(lit);
            } else {
                // source is non-literal expression
                if src.kind() == "template_string" {
                    if let Ok(t) = src.utf8_text(self.source.as_bytes()) {
                        if let Some(pre) = t.split("${").next().map(|s| s.trim_matches('`')) {
                            if !pre.is_empty() {
                                self.record_dynamic_prefix(pre);
                            }
                        }
                    }
                }
                self.dynamic_details.push(format!(
                    "{}:{}: dynamic import() with non-literal argument",
                    self.rel_path,
                    self.line(node)
                ));
                return;
            }
        } else {
            // walk for string child
            let mut cursor = node.walk();
            let children: Vec<Node> = node.children(&mut cursor).collect();
            for c in &children {
                if c.kind() == "string" || c.kind() == "string_fragment" {
                    if let Some(lit) = string_literal_value(self.source, c) {
                        found_literal = Some(lit);
                        break;
                    }
                }
            }
            // template_string with ${} => dynamic
            for c in &children {
                if c.kind() == "template_string" {
                    let t = self.text(c).to_string();
                    if t.contains("${") {
                        if let Some(pre) = t.split("${").next().map(|s| s.trim_matches('`')) {
                            if !pre.is_empty() {
                                self.record_dynamic_prefix(pre);
                            }
                        }
                        self.dynamic_details.push(format!(
                            "{}:{}: dynamic import() with template expression",
                            self.rel_path,
                            self.line(node)
                        ));
                        return;
                    } else if found_literal.is_none() {
                        found_literal = Some(t.trim_matches('`').to_string());
                    }
                }
            }
        }
        if let Some(lit) = found_literal {
            self.imports.push(ImportRec {
                from_file: self.rel_path.clone(),
                source_raw: lit,
                resolved_file: None,
                local_name: "*dynamic*".to_string(),
                original_name: "*".to_string(),
                is_type_only: false,
                kind: ImportKind::DynamicLiteral,
                line: self.line(node),
            });
        } else if node.child_by_field_name("source").is_some() {
            self.dynamic_details.push(format!(
                "{}:{}: dynamic import() with non-literal argument",
                self.rel_path,
                self.line(node)
            ));
        }
    }

    fn visit_subscript(&mut self, node: &Node) {
        // `globalThis[name]`, `window[name]`, `obj[dynamic]` — risk if index is non-literal.
        let obj = node.child_by_field_name("object");
        let idx = node.child_by_field_name("index");
        if let (Some(o), Some(i)) = (obj, idx) {
            let otext = self.text(&o).to_string();
            let ikind = i.kind();
            let is_literal = matches!(ikind, "string" | "number" | "string_fragment")
                || (ikind == "template_string" && !self.text(&i).contains("${"));
            if (otext == "globalThis"
                || otext == "window"
                || otext == "global"
                || otext == "self"
                || otext == "Reflect")
                && !is_literal
            {
                self.dynamic_details.push(format!(
                    "{}:{}: dynamic property access on {}",
                    self.rel_path,
                    self.line(node),
                    otext
                ));
            } else if otext == "Reflect" {
                // Reflect.get(...) is call path; Reflect[x] also dynamic
                self.dynamic_details.push(format!(
                    "{}:{}: Reflect dynamic access",
                    self.rel_path,
                    self.line(node)
                ));
            }
            // `handlers[key]` value reads (not just calls) dispatch dynamically:
            // any method of the base object may be selected.
            if !is_literal {
                let base_root = otext.split('.').next().unwrap_or(&otext).trim().to_string();
                if !base_root.is_empty()
                    && base_root != "this"
                    && base_root != "super"
                    && base_root != "exports"
                    && base_root != "module"
                {
                    self.dynamic_dispatch_bases
                        .push((base_root, self.current_symbol()));
                }
            }
        }
    }

    fn visit_type_node(&mut self, node: &Node) {
        if node.kind() == "type_identifier" {
            let name = self.text(node).to_string();
            if !is_builtin_name(&name) && !is_builtin_type(&name) {
                // Avoid recording the declaration name itself (parent is declaration) —
                // declaration visitors return early so this is usage.
                self.push_ref(node, &name, None, RefKind::Type, RefContext::TypePosition);
            }
        }
    }

    fn finish_escape_tracking(&mut self) {
        // `return s` where `s` holds `new Service()` escapes a Service instance.
        let types: std::collections::HashMap<&str, &str> = self
            .var_types
            .iter()
            .map(|(v, c)| (v.as_str(), c.as_str()))
            .collect();
        for ident in self.returned_idents.clone() {
            if let Some(cls) = types.get(ident.as_str()) {
                self.escaped_classes.push(cls.to_string());
            }
        }
        self.escaped_classes.sort();
        self.escaped_classes.dedup();
    }

    fn finish_local_export_marks(&mut self) {
        // Mark entities exported via `export { local }`.
        // Build map name -> indices.
        for le in self.local_exports.clone() {
            if le.local_name == "default" || le.exported_name == "default" {
                // `export default foo` — mark foo as exported+default
                for e in self.entities.iter_mut() {
                    if e.name == le.local_name && e.file == self.rel_path {
                        e.exported = true;
                        if le.is_default || le.exported_name == "default" {
                            e.is_default_export = true;
                        }
                    }
                }
                continue;
            }
            for e in self.entities.iter_mut() {
                if e.name == le.local_name && e.file == self.rel_path {
                    e.exported = true;
                }
            }
        }
    }

    fn detect_dynamic_heuristics(&mut self) {
        let src = self.source;
        let rel = &self.rel_path;
        let mut extra: Vec<String> = Vec::new();
        // Text-level heuristics for patterns tree walk may miss.
        for (i, line) in src.lines().enumerate() {
            let n = i + 1;
            let t = line.trim();
            if t.contains("Reflect.get") || t.contains("Reflect.has") || t.contains("Reflect.apply")
            {
                extra.push(format!("{}:{}: Reflect.* usage", rel, n));
            }
            if t.contains("globalThis[") || t.contains("window[") || t.contains("global[") {
                // Only flag computed (non-literal already handled); flag any to be safe if contains variable.
                if t.contains("${") || regex_simple_dynamic_index(t) {
                    extra.push(format!("{}:{}: computed global property access", rel, n));
                }
            }
            if t.starts_with("//") || t.starts_with('*') {
                continue;
            }
        }
        // Deduplicate
        for d in extra {
            if !self.dynamic_details.contains(&d) {
                self.dynamic_details.push(d);
            }
        }
    }
}

fn regex_simple_dynamic_index(line: &str) -> bool {
    // `[name]`, `[key]`, `[k]` style non-literal index on globals.
    if let (Some(a), Some(b)) = (line.find('['), line.find(']')) {
        if b > a {
            let inner = line[a + 1..b].trim();
            if !inner.is_empty()
                && !inner.starts_with('"')
                && !inner.starts_with('\'')
                && !inner.starts_with('`')
                && inner.parse::<f64>().is_err()
            {
                return true;
            }
        }
    }
    false
}

fn is_builtin_name(name: &str) -> bool {
    // Names that are syntactically IMPOSSIBLE as binding references in both
    // JavaScript and TypeScript (literals, reserved words, module syntax).
    // Everything else — even globals like `console`, `process`, `self`,
    // `type`, or `Error` — may be shadowed by a same-named declaration, and
    // scope-aware resolution picks the right one (or links nothing).
    matches!(
        name,
        "undefined"
            | "null"
            | "true"
            | "false"
            | "this"
            | "super"
            | "new"
            | "delete"
            | "void"
            | "typeof"
            | "instanceof"
            | "in"
            | "of"
            | "return"
            | "throw"
            | "break"
            | "continue"
            | "debugger"
            | "if"
            | "else"
            | "for"
            | "while"
            | "do"
            | "switch"
            | "case"
            | "default"
            | "try"
            | "catch"
            | "finally"
            | "with"
            | "import"
            | "export"
            | "function"
            | "class"
            | "extends"
            | "const"
            | "let"
            | "var"
            | "static"
            | "implements"
    )
}

fn is_builtin_type(name: &str) -> bool {
    matches!(
        name,
        "string"
            | "number"
            | "boolean"
            | "void"
            | "null"
            | "undefined"
            | "any"
            | "unknown"
            | "never"
            | "object"
            | "Record"
            | "Partial"
            | "Required"
            | "Readonly"
            | "Pick"
            | "Omit"
            | "Exclude"
            | "Extract"
            | "NonNullable"
            | "ReturnType"
            | "Parameters"
            | "Promise"
            | "Array"
            | "ReadonlyArray"
    )
}

fn is_default_export_context(source: &str, node: &Node) -> bool {
    // Walk up: if an ancestor export_statement text starts with `export default`, true.
    let mut cur = node.parent();
    while let Some(p) = cur {
        if p.kind() == "export_statement" {
            if let Ok(t) = p.utf8_text(source.as_bytes()) {
                let tt = t.trim_start();
                if tt.starts_with("export default") {
                    return true;
                }
            }
            return false;
        }
        cur = p.parent();
    }
    false
}

fn child_string_literal(source: &str, node: &Node, field: &str) -> Option<String> {
    let src_node = node.child_by_field_name(field)?;
    string_literal_value(source, &src_node)
}

fn string_literal_value(source: &str, node: &Node) -> Option<String> {
    let kind = node.kind();
    if kind == "string" {
        let t = node.utf8_text(source.as_bytes()).ok()?;
        return Some(strip_quotes(t));
    }
    if kind == "string_fragment" {
        let t = node.utf8_text(source.as_bytes()).ok()?;
        return Some(t.to_string());
    }
    // string may wrap fragment children
    let mut cursor = node.walk();
    for child in node.children(&mut cursor) {
        if child.kind() == "string_fragment" {
            if let Ok(t) = child.utf8_text(source.as_bytes()) {
                return Some(t.to_string());
            }
        }
    }
    None
}

fn strip_quotes(s: &str) -> String {
    let t = s.trim();
    if t.len() >= 2 {
        let (f, l) = (t.chars().next().unwrap(), t.chars().last().unwrap());
        if (f == '"' && l == '"') || (f == '\'' && l == '\'') || (f == '`' && l == '`') {
            return t[1..t.len() - 1].to_string();
        }
    }
    t.to_string()
}

fn first_string_arg(source: &str, args_node: &Node) -> Option<String> {
    let mut cursor = args_node.walk();
    let children: Vec<Node> = args_node.children(&mut cursor).collect();
    // Only a SOLE string argument is a literal (`require("./x")`).
    // Concatenation (`"./" + name`), templates with `${}`, or variables are dynamic.
    let significant: Vec<&Node> = children
        .iter()
        .filter(|c| !matches!(c.kind(), "(" | ")" | ","))
        .collect();
    if significant.len() != 1 {
        return None;
    }
    match significant[0].kind() {
        "string" => string_literal_value(source, significant[0]),
        "template_string" => {
            let t = significant[0].utf8_text(source.as_bytes()).ok()?;
            if t.contains("${") {
                None
            } else {
                Some(strip_quotes(t))
            }
        }
        _ => None,
    }
}

fn node_text_contains(source: &str, node: &Node, needle: &str) -> bool {
    node.utf8_text(source.as_bytes())
        .map(|t| t.contains(needle))
        .unwrap_or(false)
}

fn has_type_modifier(node: &Node) -> bool {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "type" {
            return true;
        }
    }
    false
}

fn spec_is_type(node: &Node) -> bool {
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "type" {
            return true;
        }
    }
    false
}

fn import_specifier_names(source: &str, node: &Node) -> (String, String) {
    // `foo`, `foo as bar`, `type foo`
    let mut names: Vec<String> = Vec::new();
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "identifier" {
            if let Ok(t) = c.utf8_text(source.as_bytes()) {
                names.push(t.to_string());
            }
        }
    }
    match names.as_slice() {
        [single] => (single.clone(), single.clone()),
        [orig, alias, ..] => (orig.clone(), alias.clone()),
        _ => ("?".to_string(), "?".to_string()),
    }
}

fn export_specifier_names(source: &str, node: &Node) -> (String, String) {
    // `foo`, `foo as bar`
    let mut names: Vec<String> = Vec::new();
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "identifier" || c.kind() == "property_identifier" {
            if let Ok(t) = c.utf8_text(source.as_bytes()) {
                names.push(t.to_string());
            }
        }
    }
    match names.as_slice() {
        [single] => (single.clone(), single.clone()),
        [orig, alias, ..] => (orig.clone(), alias.clone()),
        _ => ("?".to_string(), "?".to_string()),
    }
}

fn last_identifier_text(source: &str, node: &Node) -> Option<String> {
    let mut last: Option<String> = None;
    let mut cursor = node.walk();
    for c in node.children(&mut cursor) {
        if c.kind() == "identifier" {
            if let Ok(t) = c.utf8_text(source.as_bytes()) {
                last = Some(t.to_string());
            }
        } else if let Some(inner) = last_identifier_text(source, &c) {
            last = Some(inner);
        }
    }
    last
}

/// If `node` is `require("...")` with a string literal, return the specifier.
fn require_call_spec(source: &str, node: &Node) -> Option<String> {
    if node.kind() != "call_expression" {
        return None;
    }
    let func = node.child_by_field_name("function")?;
    if func.kind() != "identifier" {
        return None;
    }
    if func.utf8_text(source.as_bytes()).ok()? != "require" {
        return None;
    }
    let args = node.child_by_field_name("arguments")?;
    first_string_arg(source, &args)
}

/// First `type_identifier` inside a type annotation (e.g. `: Service` / `: Service[]`).
fn first_type_identifier(source: &str, node: &Node) -> Option<String> {
    if node.kind() == "type_identifier" {
        if let Ok(t) = node.utf8_text(source.as_bytes()) {
            if !is_builtin_type(t) {
                return Some(t.to_string());
            }
        }
        return None;
    }
    let mut cursor = node.walk();
    let children: Vec<Node> = node.children(&mut cursor).collect();
    for c in &children {
        if let Some(t) = first_type_identifier(source, c) {
            return Some(t);
        }
    }
    None
}

/// Pairs from `const {a, b: c, ...rest} = ...` pattern node: (original, local).
fn destructure_pairs(source: &str, pattern: &Node) -> Vec<(String, String)> {
    let mut out = Vec::new();
    let mut cursor = pattern.walk();
    let children: Vec<Node> = pattern.children(&mut cursor).collect();
    for c in &children {
        match c.kind() {
            "shorthand_property_identifier_pattern" | "shorthand_property_identifier" => {
                if let Ok(t) = c.utf8_text(source.as_bytes()) {
                    out.push((t.to_string(), t.to_string()));
                }
            }
            "pair_pattern" | "pair" => {
                // key: value
                let mut key: Option<String> = None;
                let mut val: Option<String> = None;
                let mut c2 = c.walk();
                let kv: Vec<Node> = c.children(&mut c2).collect();
                let mut seen_key = false;
                for k in &kv {
                    match k.kind() {
                        "property_identifier" | "string" | "number" => {
                            if !seen_key {
                                key = k.utf8_text(source.as_bytes()).ok().map(strip_quotes);
                                seen_key = true;
                            }
                        }
                        "identifier" => {
                            if seen_key {
                                val = k.utf8_text(source.as_bytes()).ok().map(|s| s.to_string());
                            } else {
                                key = k.utf8_text(source.as_bytes()).ok().map(|s| s.to_string());
                                seen_key = true;
                            }
                        }
                        _ => {}
                    }
                }
                if let (Some(k), Some(v)) = (key, val) {
                    out.push((k, v));
                }
            }
            "object_pattern" | "object" => {
                out.extend(destructure_pairs(source, c));
            }
            _ => {}
        }
    }
    // Fallback: textual parse for grammars with flat anonymous children.
    if out.is_empty() {
        if let Ok(t) = pattern.utf8_text(source.as_bytes()) {
            let inner = t.trim().trim_matches(|c| c == '{' || c == '}');
            for part in inner.split(',') {
                let part = part.trim().trim_start_matches("type ").trim();
                if part.is_empty() || part.starts_with("...") {
                    continue;
                }
                if let Some((k, v)) = part.split_once(':') {
                    out.push((k.trim().to_string(), v.trim().to_string()));
                } else {
                    let n = part.to_string();
                    out.push((n.clone(), n));
                }
            }
        }
    }
    out
}

/// Exported/local pairs from `module.exports = { f, g: h }` object literal.
fn object_literal_exports(source: &str, node: &Node) -> Vec<(String, String)> {
    // Returns (exported, local).
    destructure_pairs(source, node)
}

/// Object/index parts of a subscript: (object text, index-is-literal, literal name).
fn subscript_parts(source: &str, node: &Node) -> (Option<String>, bool, Option<String>) {
    let obj = node.child_by_field_name("object");
    let idx = node.child_by_field_name("index");
    let o = obj.and_then(|n| {
        n.utf8_text(source.as_bytes())
            .ok()
            .map(|s| s.split('.').next().unwrap_or(s).trim().to_string())
    });
    let (is_literal, lit_name) = match idx {
        Some(i) => match i.kind() {
            "string" | "string_fragment" => (true, string_literal_value(source, &i)),
            "number" => (true, None),
            "template_string" => {
                let t = i.utf8_text(source.as_bytes()).unwrap_or("");
                if t.contains("${") {
                    (false, None)
                } else {
                    (true, Some(strip_quotes(t)))
                }
            }
            _ => (false, None),
        },
        None => (false, None),
    };
    (o, is_literal, lit_name)
}

/// Evaluate `path.join(__dirname, "a", "b.js")` / `path.resolve(__dirname, ...)`
/// with all-literal segments into a relative specifier (`./a/b.js`).
/// Returns `None` when this is not such a call, `Some(None)` when a segment
/// is dynamic, `Some(Some(spec))` when statically known.
fn eval_path_call(source: &str, args_node: &Node) -> Option<Option<String>> {
    // Find the enclosing call's callee: args -> arguments -> call_expression.
    let call = args_node.parent()?;
    if call.kind() != "call_expression" {
        return None;
    }
    let func = call.child_by_field_name("function")?;
    if func.kind() != "member_expression" {
        return None;
    }
    let (_, prop) = member_parts(source, &func);
    match prop.as_deref() {
        Some("join") | Some("resolve") => {}
        _ => return None,
    }
    let mut cursor = args_node.walk();
    let kids: Vec<Node> = args_node
        .children(&mut cursor)
        .filter(|c| !matches!(c.kind(), "(" | ")" | ","))
        .collect();
    if kids.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    let mut saw_dirname = false;
    for k in &kids {
        match k.kind() {
            "identifier" => {
                let t = k.utf8_text(source.as_bytes()).unwrap_or("");
                if t == "__dirname" || t == "__filename" {
                    saw_dirname = true;
                    parts.push(".".to_string());
                } else {
                    return Some(None);
                }
            }
            "string" => {
                if let Some(lit) = string_literal_value(source, k) {
                    parts.push(lit);
                } else {
                    return Some(None);
                }
            }
            _ => return Some(None),
        }
    }
    if !saw_dirname {
        return None;
    }
    let joined = parts.join("/");
    let spec = if joined.starts_with('.') || joined.starts_with('/') {
        joined
    } else {
        format!("./{}", joined)
    };
    Some(Some(spec))
}

/// Normalize a slash-separated relative path lexically.
fn normalize_rel_slash(p: &str) -> String {
    let mut parts: Vec<&str> = Vec::new();
    for comp in p.split('/') {
        match comp {
            "" | "." => {}
            ".." => {
                parts.pop();
            }
            c => parts.push(c),
        }
    }
    parts.join("/")
}

/// Static directory prefix of a dynamic module-loading argument, relative to
/// the importer directory: template `` `./plugins/${n}` `` → `./plugins/`;
/// `path.join(__dirname, "rules", name)` → `./rules`.
/// Returns None when no static prefix exists.
fn dynamic_prefix_of_arg(source: &str, args_node: &Node) -> Option<String> {
    let mut cursor = args_node.walk();
    let kids: Vec<Node> = args_node
        .children(&mut cursor)
        .filter(|c| !matches!(c.kind(), "(" | ")" | ","))
        .collect();
    if kids.len() != 1 {
        return None;
    }
    let only = kids[0];
    // Template with interpolation.
    if only.kind() == "template_string" {
        let t = only.utf8_text(source.as_bytes()).ok()?;
        if !t.contains("${") {
            return None;
        }
        let prefix = t.split("${").next().unwrap_or("").trim_matches('`');
        if prefix.is_empty() {
            return None;
        }
        return Some(prefix.to_string());
    }
    // path.join(__dirname, "static", dynamic)
    if only.kind() == "call_expression" {
        return path_join_prefix(source, &only);
    }
    None
}

/// Static prefix of `path.join(__dirname, "static", dynamic)` /
/// `path.resolve(...)`: literal segments after `__dirname`, with trailing
/// slash (`./static/`). Bare `path.join(__dirname, name)` yields `./`
/// (the directory itself). None when not such a call.
fn path_join_prefix(source: &str, call_node: &Node) -> Option<String> {
    let func = call_node.child_by_field_name("function")?;
    if func.kind() != "member_expression" {
        return None;
    }
    let (_, prop) = member_parts(source, &func);
    match prop.as_deref() {
        Some("join") | Some("resolve") => {}
        _ => return None,
    }
    let sub = call_node.child_by_field_name("arguments")?;
    let mut c2 = sub.walk();
    let segs: Vec<Node> = sub
        .children(&mut c2)
        .filter(|c| !matches!(c.kind(), "(" | ")" | ","))
        .collect();
    let mut parts: Vec<String> = Vec::new();
    let mut saw_dirname = false;
    for s in &segs {
        match s.kind() {
            "identifier" => {
                let t = s.utf8_text(source.as_bytes()).unwrap_or("");
                if t == "__dirname" || t == "__filename" {
                    saw_dirname = true;
                    parts.push(".".to_string());
                } else {
                    break;
                }
            }
            "string" => {
                if let Some(lit) = string_literal_value(source, s) {
                    parts.push(lit);
                } else {
                    break;
                }
            }
            _ => break,
        }
    }
    if !saw_dirname || parts.is_empty() {
        return None;
    }
    let joined = parts.join("/");
    Some(if joined.ends_with('/') {
        joined
    } else {
        format!("{}/", joined.trim_end_matches('/'))
    })
}

/// Split a member expression into (object text, property text).
fn member_parts(source: &str, node: &Node) -> (Option<String>, Option<String>) {
    let obj = node.child_by_field_name("object");
    let prop = node.child_by_field_name("property");
    let o = obj.and_then(|n| {
        n.utf8_text(source.as_bytes()).ok().map(|s| {
            // For `this`, nested member etc., take first segment
            s.split('.').next().unwrap_or(s).trim().to_string()
        })
    });
    let p = prop.and_then(|n| n.utf8_text(source.as_bytes()).ok().map(|s| s.to_string()));
    (o, p)
}

#[allow(dead_code)]
fn node_debug(source: &str, node: &Node) -> String {
    format!(
        "{} [{}] {:?}",
        node.kind(),
        node.utf8_text(source.as_bytes())
            .unwrap_or("")
            .chars()
            .take(40)
            .collect::<String>(),
        node.start_position()
    )
}

pub use Language as SourceLanguage;

fn extract_star_as_name(source: &str, node: &Node) -> Option<String> {
    // `export * as ns from "..."` — find identifier between `*` and `from`.
    let text = node.utf8_text(source.as_bytes()).ok()?;
    let star = text.find('*')?;
    let after = &text[star + 1..];
    // Expect `as <name>`.
    let after = after.trim_start();
    if let Some(rest) = after.strip_prefix("as") {
        let rest = rest.trim_start();
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_' || *c == '$')
            .collect();
        if !name.is_empty() {
            return Some(name);
        }
    }
    None
}
