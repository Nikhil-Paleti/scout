//! Tree-sitter queries, one per language.
//!
//! Every pattern captures the identifier as `@name` and the whole construct
//! as `@def.<kind>` (a definition) or `@call` (a call site). When two
//! patterns capture the same name, the one written first wins, so the more
//! specific patterns (methods inside classes) come before general ones.

pub const C: &str = r#"
(function_definition declarator: (function_declarator declarator: (identifier) @name)) @def.function
(function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name))) @def.function
(function_definition declarator: (pointer_declarator declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name)))) @def.function
(declaration declarator: (function_declarator declarator: (identifier) @name)) @def.prototype
(declaration declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name))) @def.prototype
(struct_specifier name: (type_identifier) @name body: (_)) @def.struct
(union_specifier name: (type_identifier) @name body: (_)) @def.union
(enum_specifier name: (type_identifier) @name body: (_)) @def.enum
(type_definition declarator: (type_identifier) @name) @def.type
(type_definition declarator: (pointer_declarator declarator: (type_identifier) @name)) @def.type
(enumerator name: (identifier) @name) @def.constant
(preproc_def name: (identifier) @name) @def.macro
(preproc_function_def name: (identifier) @name) @def.macro
(call_expression function: (identifier) @name) @call
(call_expression function: (field_expression field: (field_identifier) @name)) @call
(call_expression function: (parenthesized_expression (pointer_expression argument: (identifier) @name))) @call
"#;

pub const CPP: &str = r#"
(function_definition declarator: (function_declarator declarator: (qualified_identifier name: (identifier) @name))) @def.method
(function_definition declarator: (function_declarator declarator: (qualified_identifier name: (qualified_identifier name: (identifier) @name)))) @def.method
(function_definition declarator: (function_declarator declarator: (qualified_identifier name: (destructor_name (identifier) @name)))) @def.method
(function_definition declarator: (function_declarator declarator: (field_identifier) @name)) @def.method
(function_definition declarator: (function_declarator declarator: (identifier) @name)) @def.function
(function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name))) @def.function
(function_definition declarator: (pointer_declarator declarator: (function_declarator declarator: (qualified_identifier name: (identifier) @name)))) @def.method
(function_definition declarator: (reference_declarator (function_declarator declarator: (identifier) @name))) @def.function
(function_definition declarator: (reference_declarator (function_declarator declarator: (qualified_identifier name: (identifier) @name)))) @def.method
(field_declaration declarator: (function_declarator declarator: (field_identifier) @name)) @def.method
(field_declaration declarator: (pointer_declarator declarator: (function_declarator declarator: (field_identifier) @name))) @def.method
(field_declaration declarator: (reference_declarator (function_declarator declarator: (field_identifier) @name))) @def.method
(declaration declarator: (function_declarator declarator: (identifier) @name)) @def.prototype
(declaration declarator: (pointer_declarator declarator: (function_declarator declarator: (identifier) @name))) @def.prototype
(class_specifier name: (type_identifier) @name body: (_)) @def.class
(struct_specifier name: (type_identifier) @name body: (_)) @def.struct
(union_specifier name: (type_identifier) @name body: (_)) @def.union
(enum_specifier name: (type_identifier) @name body: (_)) @def.enum
(namespace_definition name: (namespace_identifier) @name) @def.module
(alias_declaration name: (type_identifier) @name) @def.type
(type_definition declarator: (type_identifier) @name) @def.type
(concept_definition name: (identifier) @name) @def.type
(enumerator name: (identifier) @name) @def.constant
(preproc_def name: (identifier) @name) @def.macro
(preproc_function_def name: (identifier) @name) @def.macro
(call_expression function: (identifier) @name) @call
(call_expression function: (field_expression field: (field_identifier) @name)) @call
(call_expression function: (qualified_identifier name: (identifier) @name)) @call
(call_expression function: (qualified_identifier name: (qualified_identifier name: (identifier) @name))) @call
(call_expression function: (template_function name: (identifier) @name)) @call
(call_expression function: (qualified_identifier name: (template_function name: (identifier) @name))) @call
(call_expression function: (field_expression field: (template_method name: (field_identifier) @name))) @call
(new_expression type: (type_identifier) @name) @call
"#;

pub const RUST: &str = r#"
(declaration_list (function_item name: (identifier) @name) @def.method)
(declaration_list (function_signature_item name: (identifier) @name) @def.method)
(function_item name: (identifier) @name) @def.function
(function_signature_item name: (identifier) @name) @def.function
(struct_item name: (type_identifier) @name) @def.struct
(enum_item name: (type_identifier) @name) @def.enum
(union_item name: (type_identifier) @name) @def.union
(type_item name: (type_identifier) @name) @def.type
(trait_item name: (type_identifier) @name) @def.trait
(mod_item name: (identifier) @name) @def.module
(macro_definition name: (identifier) @name) @def.macro
(const_item name: (identifier) @name) @def.constant
(static_item name: (identifier) @name) @def.constant
(enum_variant name: (identifier) @name) @def.constant
(call_expression function: (identifier) @name) @call
(call_expression function: (field_expression field: (field_identifier) @name)) @call
(call_expression function: (scoped_identifier name: (identifier) @name)) @call
(call_expression function: (generic_function function: (identifier) @name)) @call
(call_expression function: (generic_function function: (scoped_identifier name: (identifier) @name))) @call
(call_expression function: (generic_function function: (field_expression field: (field_identifier) @name))) @call
(macro_invocation macro: (identifier) @name) @call
"#;

pub const GO: &str = r#"
(function_declaration name: (identifier) @name) @def.function
(method_declaration name: (field_identifier) @name) @def.method
(type_spec name: (type_identifier) @name type: (struct_type)) @def.struct
(type_spec name: (type_identifier) @name type: (interface_type)) @def.interface
(type_spec name: (type_identifier) @name) @def.type
(type_alias name: (type_identifier) @name) @def.type
(method_elem name: (field_identifier) @name) @def.method
(source_file (const_declaration (const_spec name: (identifier) @name) @def.constant))
(source_file (var_declaration (var_spec name: (identifier) @name) @def.variable))
(call_expression function: (identifier) @name) @call
(call_expression function: (selector_expression field: (field_identifier) @name)) @call
"#;

const PYTHON_BASE: &str = r#"
(class_definition body: (block (function_definition name: (identifier) @name) @def.method))
(class_definition body: (block (decorated_definition definition: (function_definition name: (identifier) @name) @def.method)))
(function_definition name: (identifier) @name) @def.function
(class_definition name: (identifier) @name) @def.class
((module (expression_statement (assignment left: (identifier) @name) @def.constant))
  (#match? @name "^[A-Z][A-Z0-9_]*$"))
(call function: (identifier) @name) @call
(call function: (attribute attribute: (identifier) @name)) @call
"#;

/// Starlark (Bazel BUILD files and .bzl): Python's grammar plus targets,
/// `name = "foo"` inside a rule call.
const STARLARK_EXTRA: &str = r#"
((call arguments: (argument_list (keyword_argument name: (identifier) @_k value: (string (string_content) @name)))) @def.target
  (#eq? @_k "name"))
"#;

pub fn python() -> String {
    PYTHON_BASE.to_string()
}

pub fn starlark() -> String {
    format!("{STARLARK_EXTRA}{PYTHON_BASE}")
}

pub const LUA: &str = r#"
(function_declaration name: (method_index_expression method: (identifier) @name)) @def.method
(function_declaration name: (dot_index_expression field: (identifier) @name)) @def.function
(function_declaration name: (identifier) @name) @def.function
(assignment_statement (variable_list . name: (identifier) @name) (expression_list . value: (function_definition))) @def.function
(assignment_statement (variable_list . name: (dot_index_expression field: (identifier) @name)) (expression_list . value: (function_definition))) @def.function
(field name: (identifier) @name value: (function_definition)) @def.function
(function_call name: (identifier) @name) @call
(function_call name: (dot_index_expression field: (identifier) @name)) @call
(function_call name: (method_index_expression method: (identifier) @name)) @call
"#;

const JS_COMMON: &str = r#"
(method_definition name: (property_identifier) @name) @def.method
(function_declaration name: (identifier) @name) @def.function
(generator_function_declaration name: (identifier) @name) @def.function
(function_expression name: (identifier) @name) @def.function
(variable_declarator name: (identifier) @name value: [(arrow_function) (function_expression)]) @def.function
(assignment_expression left: (member_expression property: (property_identifier) @name) right: [(arrow_function) (function_expression)]) @def.function
(assignment_expression left: (identifier) @name right: [(arrow_function) (function_expression)]) @def.function
(pair key: (property_identifier) @name value: [(arrow_function) (function_expression)]) @def.function
(call_expression function: (identifier) @name) @call
(call_expression function: (member_expression property: (property_identifier) @name)) @call
(new_expression constructor: (identifier) @name) @call
"#;

const JS_ONLY: &str = r#"
(class_declaration name: (identifier) @name) @def.class
(class name: (identifier) @name) @def.class
"#;

const TS_ONLY: &str = r#"
(class_declaration name: (type_identifier) @name) @def.class
(abstract_class_declaration name: (type_identifier) @name) @def.class
(interface_declaration name: (type_identifier) @name) @def.interface
(type_alias_declaration name: (type_identifier) @name) @def.type
(enum_declaration name: (identifier) @name) @def.enum
(internal_module name: (identifier) @name) @def.module
(function_signature name: (identifier) @name) @def.function
(method_signature name: (property_identifier) @name) @def.method
(abstract_method_signature name: (property_identifier) @name) @def.method
(public_field_definition name: (property_identifier) @name value: [(arrow_function) (function_expression)]) @def.method
"#;

pub fn javascript() -> String {
    format!("{JS_COMMON}{JS_ONLY}")
}

pub fn typescript() -> String {
    format!("{JS_COMMON}{TS_ONLY}")
}
