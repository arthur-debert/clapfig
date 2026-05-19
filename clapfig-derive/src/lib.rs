//! Procedural macro that emits `clapfig::Schema` impls from a struct
//! definition.
//!
//! Companion to the `clapfig` crate. The macro reads struct doc comments,
//! field types, doc comments, and `#[clapfig(...)]` attributes, and emits
//! a `const SchemaStatic` plus the trait impl exposing it. See
//! `docs/proposals/schema-metadata-symmetry.md` for the design intent.

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{
    Attribute, Data, DeriveInput, Expr, ExprLit, Fields, GenericArgument, Lit, Meta, PathArguments,
    Type, TypePath, parse_macro_input, spanned::Spanned,
};

/// Derive `clapfig::Schema` for a struct.
///
/// Reads field types and attributes to emit a `const`-evaluated
/// `clapfig::static_schema::SchemaStatic` tree. The generated
/// `Schema::schema()` method lazily converts it to a runtime
/// `clapfig::runtime::Schema` and caches the conversion in a per-type
/// `OnceLock`, so every existing schema consumer (JSON-Schema emitter,
/// template generator, persistence validator) sees identical metadata to
/// the runtime entry point.
///
/// # Supported field types
///
/// - Scalars: `String`, `bool`, every Rust integer type
///   (`i8`/`i16`/`i32`/`i64`/`u8`/`u16`/`u32`/`u64`/`usize`/`isize` — all
///   mapped to TOML's signed 64-bit integer; see the
///   `LeafTypeStatic::Integer` doc comment for the `i64::MAX` caveat on
///   the unsigned variants), `f32`, `f64`, `toml::value::Datetime`,
///   `toml::Value`. `i128` and `u128` are rejected at derive time.
/// - `Option<T>`: marks the leaf optional
/// - `Vec<T>` where `T` is a scalar: maps to `LeafType::Array(T)`
/// - Nested struct: assumed to also derive `clapfig::Schema`; produces a
///   `FieldStatic::Nested(...)`
///
/// # Field attributes
///
/// - `#[clapfig(default = <literal>)]` — scalar default. Accepts string,
///   integer, float, bool, and unary-negated numeric literals
///   (`-9223372036854775808i64` works for `i64::MIN`); on `Vec<T>` fields,
///   also accepts an array literal of literals. On
///   `toml::value::Datetime` fields, a string literal is emitted as
///   `ValueStatic::Datetime` (parsed at first schema access).
/// - `#[clapfig(env = "NAME")]` — explicit env-var override
/// - `#[clapfig(rename = "name")]` — override the field's schema/serde name
/// - `#[clapfig(value)]` — force `LeafType::Value` (untyped escape hatch
///   — meant for fields whose value can take multiple incompatible
///   shapes, e.g. a `#[serde(untagged)] enum`. The macro does not
///   constrain which field type this is applied to: the caller takes
///   responsibility for the deserialize side)
/// - `#[clapfig(allowed = [...])]` — set `LeafType::Enum` on a scalar
///   leaf. Works on `String`, integer, float, and `bool` fields; each
///   listed literal must match the field's TOML type, and at least one
///   value is required. Negative integer/float literals are accepted.
/// - `#[clapfig(optional)]` — force `optional = true` on a non-`Option<T>`
///   field (rarely needed; `Option<T>` is the usual spelling)
///
/// # Struct attributes
///
/// - `#[clapfig(name = "Name")]` — override the schema's name (default:
///   struct name)
/// - `#[clapfig(strict = true/false)]` — set per-node strictness for the
///   cascade
#[proc_macro_derive(Schema, attributes(clapfig))]
pub fn derive_schema(input: TokenStream) -> TokenStream {
    let input = parse_macro_input!(input as DeriveInput);
    expand_schema(input)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

fn expand_schema(input: DeriveInput) -> syn::Result<TokenStream2> {
    // Generic structs would produce a module-level `static __CLAPFIG_SCHEMA_*`
    // referencing type parameters that are not in scope for a `static`, so
    // any usage would surface as a confusing post-expansion error. Reject
    // here with a clear diagnostic.
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new(
            input.generics.span(),
            "clapfig::Schema does not support generic structs — the emitted \
             `static SchemaStatic = ...` cannot reference type parameters. \
             Concretize the type, or build the schema dynamically via \
             `Clapfig::runtime(Schema::object(...))`.",
        ));
    }
    if input.generics.where_clause.is_some() {
        return Err(syn::Error::new(
            input.generics.where_clause.span(),
            "clapfig::Schema does not support structs with a `where` clause; see \
             the generic-struct diagnostic for context.",
        ));
    }
    let struct_name = &input.ident;
    let struct_attrs = parse_struct_attrs(&input.attrs)?;
    let schema_name = struct_attrs
        .name
        .clone()
        .unwrap_or_else(|| struct_name.to_string());
    let struct_doc = collect_doc_lines(&input.attrs);

    let fields = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => &named.named,
            other => {
                return Err(syn::Error::new(
                    other.span(),
                    "clapfig::Schema requires a struct with named fields",
                ));
            }
        },
        other => {
            return Err(syn::Error::new(
                input.ident.span(),
                format!(
                    "clapfig::Schema can only be derived for structs (not {:?})",
                    discriminant(other)
                ),
            ));
        }
    };

    let mut field_entries = Vec::with_capacity(fields.len());
    for f in fields {
        let entry = expand_field(f)?;
        field_entries.push(entry);
    }

    let strict_expr = match struct_attrs.strict {
        Some(b) => quote! { Some(#b) },
        None => quote! { None },
    };
    let doc_expr = doc_slice(&struct_doc);

    let static_ident = quote::format_ident!("__CLAPFIG_SCHEMA_{}", struct_name);
    let cache_ident = quote::format_ident!("__CLAPFIG_RUNTIME_{}", struct_name);

    let output = quote! {
        #[allow(non_upper_case_globals)]
        static #static_ident: ::clapfig::static_schema::SchemaStatic =
            ::clapfig::static_schema::SchemaStatic {
                name: #schema_name,
                doc: #doc_expr,
                strict: #strict_expr,
                fields: &[ #(#field_entries),* ],
            };

        #[allow(non_upper_case_globals)]
        static #cache_ident: ::std::sync::OnceLock<
            ::std::sync::Arc<::clapfig::runtime::Schema>,
        > = ::std::sync::OnceLock::new();

        impl ::clapfig::Schema for #struct_name {
            const STATIC: &'static ::clapfig::static_schema::SchemaStatic = &#static_ident;

            fn schema() -> &'static ::clapfig::runtime::Schema {
                ::clapfig::static_schema::cached_runtime_schema(
                    &#cache_ident,
                    <Self as ::clapfig::Schema>::STATIC,
                )
            }

            fn schema_arc() -> ::std::sync::Arc<::clapfig::runtime::Schema> {
                ::clapfig::static_schema::cached_runtime_schema_arc(
                    &#cache_ident,
                    <Self as ::clapfig::Schema>::STATIC,
                )
            }
        }
    };

    Ok(output)
}

fn discriminant(data: &Data) -> &'static str {
    match data {
        Data::Struct(_) => "struct",
        Data::Enum(_) => "enum",
        Data::Union(_) => "union",
    }
}

#[derive(Default)]
struct StructAttrs {
    name: Option<String>,
    strict: Option<bool>,
}

fn parse_struct_attrs(attrs: &[Attribute]) -> syn::Result<StructAttrs> {
    let mut out = StructAttrs::default();
    for attr in attrs {
        if !attr.path().is_ident("clapfig") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("name") {
                let value: syn::LitStr = meta.value()?.parse()?;
                out.name = Some(value.value());
                Ok(())
            } else if meta.path.is_ident("strict") {
                let value: syn::LitBool = meta.value()?.parse()?;
                out.strict = Some(value.value());
                Ok(())
            } else {
                Err(meta.error(format!(
                    "unsupported #[clapfig(...)] struct attribute: `{}`. \
                     Supported: name = \"...\", strict = true/false",
                    meta.path
                        .get_ident()
                        .map(|i| i.to_string())
                        .unwrap_or_else(|| "?".into())
                )))
            }
        })?;
    }
    Ok(out)
}

#[derive(Default)]
struct FieldAttrs {
    default: Option<Expr>,
    env: Option<String>,
    rename: Option<String>,
    force_value: bool,
    allowed: Option<Vec<Expr>>,
    optional: bool,
}

fn parse_field_attrs(attrs: &[Attribute]) -> syn::Result<FieldAttrs> {
    let mut out = FieldAttrs::default();
    for attr in attrs {
        if !attr.path().is_ident("clapfig") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("default") {
                let value: Expr = meta.value()?.parse()?;
                out.default = Some(value);
                Ok(())
            } else if meta.path.is_ident("env") {
                let value: syn::LitStr = meta.value()?.parse()?;
                out.env = Some(value.value());
                Ok(())
            } else if meta.path.is_ident("rename") {
                let value: syn::LitStr = meta.value()?.parse()?;
                out.rename = Some(value.value());
                Ok(())
            } else if meta.path.is_ident("value") {
                out.force_value = true;
                Ok(())
            } else if meta.path.is_ident("optional") {
                out.optional = true;
                Ok(())
            } else if meta.path.is_ident("allowed") {
                let expr: Expr = meta.value()?.parse()?;
                let items = match expr {
                    Expr::Array(a) => a.elems.into_iter().collect(),
                    other => {
                        return Err(syn::Error::new(
                            other.span(),
                            "`allowed = [...]` requires an array literal of TOML primitives",
                        ));
                    }
                };
                out.allowed = Some(items);
                Ok(())
            } else {
                Err(meta.error(format!(
                    "unsupported #[clapfig(...)] field attribute: `{}`. \
                     Supported: default, env, rename, value, optional, allowed",
                    meta.path
                        .get_ident()
                        .map(|i| i.to_string())
                        .unwrap_or_else(|| "?".into())
                )))
            }
        })?;
    }
    Ok(out)
}

fn collect_doc_lines(attrs: &[Attribute]) -> Vec<String> {
    let mut out = Vec::new();
    for attr in attrs {
        if !attr.path().is_ident("doc") {
            continue;
        }
        if let Meta::NameValue(nv) = &attr.meta
            && let Expr::Lit(ExprLit {
                lit: Lit::Str(s), ..
            }) = &nv.value
        {
            out.push(s.value().trim().to_string());
        }
    }
    out
}

fn doc_slice(lines: &[String]) -> TokenStream2 {
    if lines.is_empty() {
        quote! { &[] }
    } else {
        let lits = lines.iter().map(|s| quote! { #s });
        quote! { &[ #(#lits),* ] }
    }
}

/// Classification of a Rust field type into the schema shape it produces.
enum TypeShape {
    /// Plain scalar leaf — carries both the emit-time token expression and
    /// the compile-time `ScalarKind` discriminant. The kind lets attribute
    /// validators (e.g. `#[clapfig(allowed = [...])]`) check that each
    /// allowed literal's TOML type matches the field's scalar kind.
    Scalar(ScalarKind, TokenStream2),
    /// `Option<T>` where T is itself a TypeShape; the inner shape is folded
    /// and `optional` is set.
    Optional(Box<TypeShape>),
    /// `Vec<T>` where T is a scalar — emits `LeafType::Array(T)`.
    Array(TokenStream2),
    /// Nested type referencing another struct's `clapfig::Schema` impl.
    Nested(TokenStream2),
    /// `toml::Value` — emits `LeafType::Value`.
    Value,
}

/// Compile-time-discriminant mirror of the scalar `LeafTypeStatic` variants.
/// Used to validate that `#[clapfig(allowed = [...])]` literals match the
/// field's inferred type.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ScalarKind {
    String,
    Integer,
    Float,
    Bool,
    DateTime,
}

impl ScalarKind {
    fn human(self) -> &'static str {
        match self {
            ScalarKind::String => "String",
            ScalarKind::Integer => "Integer",
            ScalarKind::Float => "Float",
            ScalarKind::Bool => "Bool",
            ScalarKind::DateTime => "DateTime",
        }
    }
}

fn classify_type(ty: &Type) -> syn::Result<TypeShape> {
    let path = match ty {
        Type::Path(TypePath { path, qself: None }) => path,
        other => {
            return Err(syn::Error::new(
                other.span(),
                "clapfig::Schema only supports plain type paths (no references, tuples, etc.). \
                 Use #[clapfig(value)] for free-form values.",
            ));
        }
    };

    let last = path
        .segments
        .last()
        .ok_or_else(|| syn::Error::new(path.span(), "empty type path is not supported"))?;
    let name = last.ident.to_string();

    // `Option<T>` and `Vec<T>` are recognized by their last-segment name —
    // qualified paths like `std::option::Option<T>` are accepted because we
    // only check the final segment.
    if name == "Option" {
        let inner = single_generic_argument(&last.arguments, "Option")?;
        let inner_shape = classify_type(inner)?;
        return Ok(TypeShape::Optional(Box::new(inner_shape)));
    }
    if name == "Vec" {
        let inner = single_generic_argument(&last.arguments, "Vec")?;
        // Vec<T>: T must be a scalar shape for the leaf-array case. Nested
        // arrays-of-structs would be `Field::ArrayOf` but we defer that to
        // a follow-up (the macro would need to know whether T derives
        // Schema, which we can't tell syntactically without name
        // resolution).
        let inner_shape = classify_type(inner)?;
        return match inner_shape {
            TypeShape::Scalar(_, tok) => Ok(TypeShape::Array(tok)),
            TypeShape::Optional(_) => Err(syn::Error::new(
                inner.span(),
                "Vec<Option<T>> is not supported; use Option<Vec<T>> instead",
            )),
            TypeShape::Array(_) => Err(syn::Error::new(
                inner.span(),
                "nested arrays (Vec<Vec<...>>) are not supported by clapfig::Schema",
            )),
            TypeShape::Nested(_) => Err(syn::Error::new(
                inner.span(),
                "Vec<NestedStruct> is not yet supported by clapfig::Schema (planned: \
                 Field::ArrayOf). Use the runtime path or `#[clapfig(value)]` for now.",
            )),
            TypeShape::Value => Err(syn::Error::new(
                inner.span(),
                "Vec<toml::Value> is not supported; use a single `toml::Value` with \
                 #[clapfig(value)] instead",
            )),
        };
    }

    if name == "Value" && is_toml_value_path(path) {
        return Ok(TypeShape::Value);
    }
    if name == "Datetime" && is_toml_datetime_path(path) {
        return Ok(TypeShape::Scalar(
            ScalarKind::DateTime,
            quote! { ::clapfig::static_schema::LeafTypeStatic::DateTime },
        ));
    }

    // 128-bit integers don't fit TOML's signed-64-bit integer width and there
    // is no faithful intermediate representation. Reject at derive time with
    // a clear diagnostic rather than letting the field fall through to the
    // nested-struct branch and produce an opaque trait-bound error.
    if matches!(name.as_str(), "i128" | "u128") {
        return Err(syn::Error::new(
            ty.span(),
            format!(
                "clapfig::Schema does not support `{name}` field types: TOML's integer \
                 width is signed 64-bit and 128-bit values cannot be represented faithfully. \
                 Store as `String` and parse explicitly, or use `#[clapfig(value)]` with \
                 `toml::Value` for a free-form leaf."
            ),
        ));
    }
    let scalar = match name.as_str() {
        "String" => Some((
            ScalarKind::String,
            quote! { ::clapfig::static_schema::LeafTypeStatic::String },
        )),
        "bool" => Some((
            ScalarKind::Bool,
            quote! { ::clapfig::static_schema::LeafTypeStatic::Bool },
        )),
        // Every Rust integer maps to TOML's single Integer width.
        // `u64` / `usize` / `isize` values that exceed `i64::MAX` cannot be
        // represented in TOML; documented on `LeafTypeStatic::Integer`.
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize" => Some((
            ScalarKind::Integer,
            quote! { ::clapfig::static_schema::LeafTypeStatic::Integer },
        )),
        "f32" | "f64" => Some((
            ScalarKind::Float,
            quote! { ::clapfig::static_schema::LeafTypeStatic::Float },
        )),
        _ => None,
    };
    if let Some((kind, tok)) = scalar {
        return Ok(TypeShape::Scalar(kind, tok));
    }

    // Default: treat as a nested struct that also implements clapfig::Schema.
    // Use the associated const STATIC (not schema_static()) so the parent's
    // `static SchemaStatic = ...` initializer can compose it in const
    // context — trait fns are not callable from const on stable Rust.
    let nested = quote! { <#ty as ::clapfig::Schema>::STATIC };
    Ok(TypeShape::Nested(nested))
}

fn single_generic_argument<'a>(args: &'a PathArguments, parent: &str) -> syn::Result<&'a Type> {
    let abga = match args {
        PathArguments::AngleBracketed(a) => a,
        _ => {
            return Err(syn::Error::new(
                args.span(),
                format!("{parent} requires a single type argument"),
            ));
        }
    };
    if abga.args.len() != 1 {
        return Err(syn::Error::new(
            abga.span(),
            format!("{parent} requires exactly one type argument"),
        ));
    }
    match abga.args.first().unwrap() {
        GenericArgument::Type(t) => Ok(t),
        other => Err(syn::Error::new(
            other.span(),
            format!("{parent}'s type argument must be a type"),
        )),
    }
}

fn is_toml_value_path(path: &syn::Path) -> bool {
    // Match `toml::Value`, `Value` (re-imported), `::toml::Value`.
    // Last segment is "Value"; if there's a preceding segment it must be
    // exactly "toml" — guards against unrelated `Value` types being
    // accidentally picked up.
    let segs: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    match segs.as_slice() {
        [last] => last == "Value",
        [.., a, b] => b == "Value" && a == "toml",
        _ => false,
    }
}

fn is_toml_datetime_path(path: &syn::Path) -> bool {
    // Match exactly: `Datetime` (single segment — assumed to be a use-imported
    // `toml::value::Datetime`), `toml::value::Datetime` (the canonical form),
    // or `value::Datetime` only when the immediately preceding segment is
    // `toml`. An unrelated `my_crate::value::Datetime` would have a different
    // root segment and is correctly rejected here.
    let segs: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    let last_is_datetime = segs.last().map(|s| s == "Datetime").unwrap_or(false);
    if !last_is_datetime {
        return false;
    }
    match segs.as_slice() {
        [_] => true,
        [.., a, _b] => a == "value" && segs.iter().any(|s| s == "toml"),
        _ => false,
    }
}

fn expand_field(field: &syn::Field) -> syn::Result<TokenStream2> {
    let ident = field
        .ident
        .as_ref()
        .ok_or_else(|| syn::Error::new(field.span(), "expected named field"))?;
    let attrs = parse_field_attrs(&field.attrs)?;
    let doc_lines = collect_doc_lines(&field.attrs);
    let name = attrs.rename.clone().unwrap_or_else(|| ident.to_string());
    let doc_expr = doc_slice(&doc_lines);

    let shape = classify_type(&field.ty)?;

    // Nested struct → FieldStatic::Nested. Field-level attrs (default, env,
    // allowed, value) are leaf-only; reject them.
    if let TypeShape::Nested(inner_expr) = &shape {
        if attrs.default.is_some()
            || attrs.env.is_some()
            || attrs.force_value
            || attrs.allowed.is_some()
            || attrs.optional
        {
            return Err(syn::Error::new(
                field.span(),
                "leaf attributes (default, env, value, allowed, optional) are not \
                 valid on nested-struct fields",
            ));
        }
        return Ok(quote! {
            ::clapfig::static_schema::NamedFieldStatic {
                name: #name,
                field: ::clapfig::static_schema::FieldStatic::Nested(#inner_expr),
            }
        });
    }

    // For everything else we build a Leaf.
    let (leaf_type_expr, optional_from_type) = leaf_type_for_shape(&shape, &attrs, field.span())?;
    let optional = attrs.optional || optional_from_type;

    let default_expr = match &attrs.default {
        Some(expr) => {
            let v = expr_to_value_static(expr, &shape)?;
            quote! { Some(#v) }
        }
        None => quote! { None },
    };

    let env_expr = match &attrs.env {
        Some(s) => quote! { Some(#s) },
        None => quote! { None },
    };

    let leaf = quote! {
        ::clapfig::static_schema::LeafStatic {
            doc: #doc_expr,
            ty: #leaf_type_expr,
            default: #default_expr,
            optional: #optional,
            env: #env_expr,
        }
    };

    Ok(quote! {
        ::clapfig::static_schema::NamedFieldStatic {
            name: #name,
            field: ::clapfig::static_schema::FieldStatic::Leaf(#leaf),
        }
    })
}

/// Compute the `LeafTypeStatic` expression for a field's shape, taking
/// `#[clapfig(value)]` and `#[clapfig(allowed = [...])]` into account.
/// Returns `(leaf_type_expr, optional_from_type)`.
fn leaf_type_for_shape(
    shape: &TypeShape,
    attrs: &FieldAttrs,
    span: proc_macro2::Span,
) -> syn::Result<(TokenStream2, bool)> {
    // `value` and `allowed` override the inferred type.
    if attrs.force_value {
        if attrs.allowed.is_some() {
            return Err(syn::Error::new(
                span,
                "`value` and `allowed` are mutually exclusive on the same field",
            ));
        }
        let (_, optional_from_type) = inner_leaf_type(shape)?;
        return Ok((
            quote! { ::clapfig::static_schema::LeafTypeStatic::Value },
            optional_from_type,
        ));
    }
    if let Some(allowed) = &attrs.allowed {
        // `allowed` constrains the field to a scalar-enum set. Permitting
        // it on Vec/Map/Value leaves would emit a schema that can never
        // validate or deserialize correctly (the value shape and the enum
        // constraint disagree). Reject early with a clear diagnostic.
        if !shape_accepts_allowed(shape) {
            return Err(syn::Error::new(
                span,
                "`#[clapfig(allowed = [...])]` is only valid on scalar leaf fields \
                 (String, integer, float, bool). It cannot be applied to Vec<T>, \
                 nested structs, or `#[clapfig(value)]` fields.",
            ));
        }
        // An empty allowed set produces a leaf that can never be satisfied
        // (no value passes the enum check) and a JSON Schema with no
        // `type` (since `leaf_type_json_name` for Enum reads the first
        // allowed value). Refuse to emit it.
        if allowed.is_empty() {
            return Err(syn::Error::new(
                span,
                "`#[clapfig(allowed = [...])]` requires at least one value; \
                 an empty set produces a leaf that can never validate.",
            ));
        }
        // The field's scalar kind drives literal-type validation: an
        // integer field with `allowed = [\"a\"]` would emit a schema and
        // deserialize that can never agree.
        let kind = scalar_kind_of(shape).expect("scalar shape after shape_accepts_allowed");
        let value_statics = allowed
            .iter()
            .map(|e| value_static_from_expr_with_kind(e, kind))
            .collect::<syn::Result<Vec<_>>>()?;
        let (_, optional_from_type) = inner_leaf_type(shape)?;
        return Ok((
            quote! {
                ::clapfig::static_schema::LeafTypeStatic::Enum {
                    values: &[ #(#value_statics),* ],
                }
            },
            optional_from_type,
        ));
    }
    inner_leaf_type(shape)
}

/// `allowed = [...]` is only meaningful on scalar leaves; otherwise the
/// emitted schema would be self-contradictory (enum-of-string constraint
/// on a Vec field, etc.).
fn shape_accepts_allowed(shape: &TypeShape) -> bool {
    match shape {
        TypeShape::Scalar(_, _) => true,
        TypeShape::Optional(inner) => shape_accepts_allowed(inner),
        TypeShape::Array(_) | TypeShape::Value | TypeShape::Nested(_) => false,
    }
}

/// Detect whether the field's classified shape is `LeafTypeStatic::DateTime`
/// (or `Option<DateTime>`). Used to route string-literal defaults to
/// `ValueStatic::Datetime` instead of `ValueStatic::String`.
fn is_datetime_shape(shape: &TypeShape) -> bool {
    matches!(scalar_kind_of(shape), Some(ScalarKind::DateTime))
}

fn inner_leaf_type(shape: &TypeShape) -> syn::Result<(TokenStream2, bool)> {
    match shape {
        TypeShape::Scalar(_, tok) => Ok((tok.clone(), false)),
        TypeShape::Optional(inner) => {
            let (inner_tok, _) = inner_leaf_type(inner)?;
            Ok((inner_tok, true))
        }
        TypeShape::Array(elem) => Ok((
            quote! { ::clapfig::static_schema::LeafTypeStatic::Array(&#elem) },
            false,
        )),
        TypeShape::Value => Ok((
            quote! { ::clapfig::static_schema::LeafTypeStatic::Value },
            false,
        )),
        TypeShape::Nested(_) => unreachable!("nested handled before leaf-type dispatch"),
    }
}

/// Extract the scalar kind from a (possibly `Option`-wrapped) scalar shape.
/// Returns `None` for non-scalar shapes — used by `allowed`-attribute
/// validation to check that literal types match the field's TOML kind.
fn scalar_kind_of(shape: &TypeShape) -> Option<ScalarKind> {
    match shape {
        TypeShape::Scalar(k, _) => Some(*k),
        TypeShape::Optional(inner) => scalar_kind_of(inner),
        _ => None,
    }
}

/// Parse a literal-or-negated-literal expression into a `ValueStatic`
/// token, without kind validation. Used inside array-literal defaults
/// where the element type is already constrained by the field's `Vec<T>`
/// declaration (the per-element kind check is done by the toml
/// deserializer at load time, not here).
fn value_static_from_expr(expr: &Expr) -> syn::Result<TokenStream2> {
    match expr {
        Expr::Lit(ExprLit { lit, .. }) => lit_to_value_static(lit, expr.span()),
        Expr::Unary(syn::ExprUnary {
            op: syn::UnOp::Neg(_),
            expr: inner,
            ..
        }) => {
            if let Expr::Lit(ExprLit { lit, .. }) = inner.as_ref() {
                negated_lit_to_value_static(lit, expr.span())
            } else {
                Err(syn::Error::new(
                    expr.span(),
                    "literal-array entries must be literal TOML primitives",
                ))
            }
        }
        _ => Err(syn::Error::new(
            expr.span(),
            "literal-array entries must be literal TOML primitives \
             (string, integer, float, bool)",
        )),
    }
}

/// Parse an `allowed = [...]` entry against the field's scalar kind.
///
/// Accepts positive literals (`"x"`, `1`, `1.5`, `true`) and unary-negated
/// numeric literals (`-1`, `-1.5`). Rejects literals whose TOML primitive
/// type doesn't match the field — e.g. `allowed = ["a"]` on an `i64` field
/// — so the emitted schema is consistent with what the deserializer can
/// accept.
fn value_static_from_expr_with_kind(expr: &Expr, kind: ScalarKind) -> syn::Result<TokenStream2> {
    let (tok, literal_kind) = match expr {
        Expr::Lit(ExprLit { lit, .. }) => (
            lit_to_value_static(lit, expr.span())?,
            lit_to_scalar_kind(lit, expr.span())?,
        ),
        Expr::Unary(syn::ExprUnary {
            op: syn::UnOp::Neg(_),
            expr: inner,
            ..
        }) => {
            if let Expr::Lit(ExprLit { lit, .. }) = inner.as_ref() {
                (
                    negated_lit_to_value_static(lit, expr.span())?,
                    lit_to_scalar_kind(lit, expr.span())?,
                )
            } else {
                return Err(syn::Error::new(
                    expr.span(),
                    "`allowed = [...]` entries must be literal TOML primitives",
                ));
            }
        }
        _ => {
            return Err(syn::Error::new(
                expr.span(),
                "`allowed = [...]` entries must be literal TOML primitives \
                 (string, integer, float, bool)",
            ));
        }
    };
    if literal_kind != kind {
        return Err(syn::Error::new(
            expr.span(),
            format!(
                "`allowed = [...]` entry has type `{}` but the field is `{}`; \
                 enum-constraint literals must match the field's TOML type.",
                literal_kind.human(),
                kind.human()
            ),
        ));
    }
    Ok(tok)
}

fn lit_to_scalar_kind(lit: &Lit, span: proc_macro2::Span) -> syn::Result<ScalarKind> {
    match lit {
        Lit::Str(_) => Ok(ScalarKind::String),
        Lit::Bool(_) => Ok(ScalarKind::Bool),
        Lit::Int(_) => Ok(ScalarKind::Integer),
        Lit::Float(_) => Ok(ScalarKind::Float),
        _ => Err(syn::Error::new(
            span,
            "literal must be a string, integer, float, or bool",
        )),
    }
}

/// Materialize a `ValueStatic` for a `#[clapfig(default = ...)]` expression.
///
/// `shape` is the field's classified type; used to validate scalar/array
/// compatibility and to drive nesting on array defaults.
fn expr_to_value_static(expr: &Expr, shape: &TypeShape) -> syn::Result<TokenStream2> {
    // Datetime fields with a string-literal default need a typed
    // `ValueStatic::Datetime` so the runtime's `LeafType::DateTime` check
    // accepts the default at finalize. Without this branch the literal
    // would emit `ValueStatic::String` and the leaf type-check would
    // reject the default at startup. We route the literal verbatim into
    // `ValueStatic::Datetime`; the parse happens inside
    // `ValueStatic::to_toml()` on first schema access and a malformed
    // literal panics with `"clapfig: invalid datetime literal in static
    // schema default"`. (Compile-time parsing would require pulling
    // `toml` / `toml_datetime` into `clapfig-derive`, which we deliberately
    // avoid to keep the proc-macro crate light.)
    if is_datetime_shape(shape)
        && let Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) = expr
    {
        let value = s.value();
        return Ok(quote! { ::clapfig::static_schema::ValueStatic::Datetime(#value) });
    }
    match expr {
        Expr::Lit(ExprLit { lit, .. }) => lit_to_value_static(lit, expr.span()),
        Expr::Array(a) => {
            let inner_shape = match shape {
                TypeShape::Array(_) => None,
                TypeShape::Optional(inner) => match inner.as_ref() {
                    TypeShape::Array(_) => None,
                    _ => Some(()),
                },
                _ => Some(()),
            };
            if inner_shape.is_some() {
                return Err(syn::Error::new(
                    expr.span(),
                    "array-literal defaults are only valid on Vec<T> (or Option<Vec<T>>) fields",
                ));
            }
            let items: Vec<TokenStream2> = a
                .elems
                .iter()
                .map(value_static_from_expr)
                .collect::<syn::Result<_>>()?;
            Ok(quote! {
                ::clapfig::static_schema::ValueStatic::Array(&[ #(#items),* ])
            })
        }
        Expr::Unary(syn::ExprUnary {
            op: syn::UnOp::Neg(_),
            expr: inner,
            ..
        }) => {
            if let Expr::Lit(ExprLit { lit, .. }) = inner.as_ref() {
                negated_lit_to_value_static(lit, expr.span())
            } else {
                Err(syn::Error::new(
                    expr.span(),
                    "unary `-` on non-literal default expressions is not supported",
                ))
            }
        }
        other => Err(syn::Error::new(
            other.span(),
            "default expression must be a literal (string, integer, float, bool) or \
             an array literal of literals. For complex defaults use the runtime path.",
        )),
    }
}

fn lit_to_value_static(lit: &Lit, span: proc_macro2::Span) -> syn::Result<TokenStream2> {
    match lit {
        Lit::Str(s) => {
            let v = s.value();
            Ok(quote! { ::clapfig::static_schema::ValueStatic::String(#v) })
        }
        Lit::Bool(b) => {
            let v = b.value();
            Ok(quote! { ::clapfig::static_schema::ValueStatic::Bool(#v) })
        }
        Lit::Int(i) => {
            let v: i64 = i.base10_parse().map_err(|e| syn::Error::new(span, e))?;
            Ok(quote! { ::clapfig::static_schema::ValueStatic::Integer(#v) })
        }
        Lit::Float(f) => {
            let v: f64 = f.base10_parse().map_err(|e| syn::Error::new(span, e))?;
            Ok(quote! { ::clapfig::static_schema::ValueStatic::Float(#v) })
        }
        _ => Err(syn::Error::new(
            span,
            "default literal must be a string, integer, float, or bool",
        )),
    }
}

fn negated_lit_to_value_static(lit: &Lit, span: proc_macro2::Span) -> syn::Result<TokenStream2> {
    match lit {
        Lit::Int(i) => {
            // Parse the magnitude as `u64`, then negate through `i128`
            // before fitting back into `i64`. Required for `i64::MIN`:
            // the user writes `-9223372036854775808` and the inner token
            // is the positive `9223372036854775808`, which overflows
            // `i64::MAX` (the lexer doesn't know the unary `-` is part
            // of the value). `u64` holds it, and `-(value as i128)`
            // exactly equals `i64::MIN` for that input.
            let raw: u64 = i
                .base10_parse()
                .map_err(|e| syn::Error::new(span, format!("integer literal: {e}")))?;
            let neg_i128 = -(raw as i128);
            let neg: i64 = i64::try_from(neg_i128).map_err(|_| {
                syn::Error::new(
                    span,
                    "negated integer literal exceeds the i64 range (TOML's integer width)",
                )
            })?;
            Ok(quote! { ::clapfig::static_schema::ValueStatic::Integer(#neg) })
        }
        Lit::Float(f) => {
            let v: f64 = f.base10_parse().map_err(|e| syn::Error::new(span, e))?;
            let neg = -v;
            Ok(quote! { ::clapfig::static_schema::ValueStatic::Float(#neg) })
        }
        _ => Err(syn::Error::new(
            span,
            "unary `-` is only valid on integer or float literals",
        )),
    }
}
