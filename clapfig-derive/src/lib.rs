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
/// - Scalars: `String`, `i8..i64`, `u8..u32`, `f32`, `f64`, `bool`,
///   `toml::value::Datetime`, `toml::Value`
/// - `Option<T>`: marks the leaf optional
/// - `Vec<T>` where `T` is a scalar: maps to `LeafType::Array(T)`
/// - Nested struct: assumed to also derive `clapfig::Schema`; produces a
///   `FieldStatic::Nested(...)`
///
/// # Field attributes
///
/// - `#[clapfig(default = <literal>)]` — scalar default
/// - `#[clapfig(env = "NAME")]` — explicit env-var override
/// - `#[clapfig(rename = "name")]` — override the field's schema/serde name
/// - `#[clapfig(value)]` — force `LeafType::Value` (untyped escape hatch)
/// - `#[clapfig(allowed = ["a", "b", "c"])]` — set `LeafType::Enum` for
///   `String` leaves with a fixed value set
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
        static #cache_ident: ::std::sync::OnceLock<::clapfig::runtime::Schema> =
            ::std::sync::OnceLock::new();

        impl ::clapfig::Schema for #struct_name {
            const STATIC: &'static ::clapfig::static_schema::SchemaStatic = &#static_ident;

            fn schema() -> &'static ::clapfig::runtime::Schema {
                ::clapfig::static_schema::cached_runtime_schema(
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
    /// Plain scalar leaf with the given leaf-type token expression.
    Scalar(TokenStream2),
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
            TypeShape::Scalar(tok) => Ok(TypeShape::Array(tok)),
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
        "String" => Some(quote! { ::clapfig::static_schema::LeafTypeStatic::String }),
        "bool" => Some(quote! { ::clapfig::static_schema::LeafTypeStatic::Bool }),
        // Every Rust integer maps to TOML's single Integer width.
        // `u64` / `usize` / `isize` values that exceed `i64::MAX` cannot be
        // represented in TOML; documented on `LeafTypeStatic::Integer`.
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize" => {
            Some(quote! { ::clapfig::static_schema::LeafTypeStatic::Integer })
        }
        "f32" | "f64" => Some(quote! { ::clapfig::static_schema::LeafTypeStatic::Float }),
        _ => None,
    };
    if let Some(tok) = scalar {
        return Ok(TypeShape::Scalar(tok));
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
    let segs: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    matches!(segs.as_slice(),
        [last] if last == "Datetime"
    ) || segs
        .windows(2)
        .any(|w| w[0] == "value" && w[1] == "Datetime")
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
        let value_statics = allowed
            .iter()
            .map(value_static_from_expr)
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

fn inner_leaf_type(shape: &TypeShape) -> syn::Result<(TokenStream2, bool)> {
    match shape {
        TypeShape::Scalar(tok) => Ok((tok.clone(), false)),
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

fn value_static_from_expr(expr: &Expr) -> syn::Result<TokenStream2> {
    if let Expr::Lit(ExprLit { lit, .. }) = expr {
        return lit_to_value_static(lit, expr.span());
    }
    Err(syn::Error::new(
        expr.span(),
        "`allowed = [...]` entries must be literal TOML primitives (string, integer, float, bool)",
    ))
}

/// Materialize a `ValueStatic` for a `#[clapfig(default = ...)]` expression.
///
/// `shape` is the field's classified type; used to validate scalar/array
/// compatibility and to drive nesting on array defaults.
fn expr_to_value_static(expr: &Expr, shape: &TypeShape) -> syn::Result<TokenStream2> {
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
            let v: i64 = i.base10_parse().map_err(|e| syn::Error::new(span, e))?;
            let neg = -v;
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
