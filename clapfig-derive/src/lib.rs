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
    Attribute, Data, DataEnum, DeriveInput, Expr, ExprLit, Fields, GenericArgument, Lit, Meta,
    PathArguments, Type, TypePath, Variant, parse_macro_input, spanned::Spanned,
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
///   `ValueStatic::Datetime`.
///
///   **Datetime caveat:** datetime defaults are *not* parsed at derive
///   time — the macro intentionally avoids pulling the `toml` parser
///   into its dependency tree. A malformed datetime literal (e.g.
///   `default = "not-a-date"` on a `Datetime` field) compiles
///   successfully and panics with `"clapfig: invalid datetime literal
///   in static schema default"` the first time `Schema::schema()` is
///   called (typically at app startup). Verify your datetime defaults
///   match TOML's grammar (RFC 3339 offset / local datetime / local
///   date / local time) before shipping.
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
            "clapfig::Schema does not support generic types — the emitted \
             `static SchemaStatic = ...` cannot reference type parameters. \
             Concretize the type, or build the schema dynamically via \
             `Clapfig::runtime(Schema::object(...))`.",
        ));
    }
    if input.generics.where_clause.is_some() {
        return Err(syn::Error::new(
            input.generics.where_clause.span(),
            "clapfig::Schema does not support types with a `where` clause; see \
             the generic-type diagnostic for context.",
        ));
    }
    let type_name = &input.ident;
    let struct_attrs = parse_struct_attrs(&input.attrs)?;
    let schema_name = struct_attrs
        .name
        .clone()
        .unwrap_or_else(|| type_name.to_string());
    let type_doc = collect_doc_lines(&input.attrs);

    let (fields_body, enum_variants_body) = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => {
                let mut field_entries = Vec::with_capacity(named.named.len());
                for f in &named.named {
                    field_entries.push(expand_field(f)?);
                }
                (quote! { &[ #(#field_entries),* ] }, quote! { &[] })
            }
            other => {
                return Err(syn::Error::new(
                    other.span(),
                    "clapfig::Schema requires a struct with named fields",
                ));
            }
        },
        Data::Enum(e) => {
            let variants = expand_enum_variants(&input.attrs, &struct_attrs, e)?;
            (quote! { &[] }, quote! { &[ #(#variants),* ] })
        }
        other => {
            return Err(syn::Error::new(
                input.ident.span(),
                format!(
                    "clapfig::Schema can only be derived for structs and unit-only enums (not {:?})",
                    discriminant(other)
                ),
            ));
        }
    };

    let strict_expr = match struct_attrs.strict {
        Some(b) => quote! { Some(#b) },
        None => quote! { None },
    };
    let doc_expr = doc_slice(&type_doc);

    let static_ident = quote::format_ident!("__CLAPFIG_SCHEMA_{}", type_name);
    let cache_ident = quote::format_ident!("__CLAPFIG_RUNTIME_{}", type_name);

    let output = quote! {
        #[allow(non_upper_case_globals)]
        static #static_ident: ::clapfig::static_schema::SchemaStatic =
            ::clapfig::static_schema::SchemaStatic {
                name: #schema_name,
                doc: #doc_expr,
                strict: #strict_expr,
                fields: #fields_body,
                enum_variants: #enum_variants_body,
            };

        #[allow(non_upper_case_globals)]
        static #cache_ident: ::std::sync::OnceLock<
            ::std::sync::Arc<::clapfig::runtime::Schema>,
        > = ::std::sync::OnceLock::new();

        impl ::clapfig::Schema for #type_name {
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

/// Walk a unit-only enum and emit `&'static str` tokens for each variant's
/// schema-facing name. Errors at derive time on non-unit variants — clapfig's
/// `LeafType::Enum` is value-shape-flat (variants carry no payload), so a
/// `Newtype(T)` / `Tuple(T, U)` / struct-form variant has no faithful
/// representation. Callers needing union shapes can opt into
/// `#[clapfig(value)]` on the field instead.
///
/// Variant names are rewritten through `#[clapfig(rename_all = "...")]` /
/// `#[serde(rename_all = "...")]` on the enum, and per-variant
/// `#[clapfig(rename = "name")]` / `#[serde(rename = "name")]` overrides
/// take precedence over the global rule. The serde forms are accepted for
/// migration convenience — the same enum can derive both `Schema` and
/// `Deserialize` without restating the rename rule.
fn expand_enum_variants(
    type_attrs: &[Attribute],
    struct_attrs: &StructAttrs,
    data: &DataEnum,
) -> syn::Result<Vec<TokenStream2>> {
    if data.variants.is_empty() {
        return Err(syn::Error::new(
            data.variants.span(),
            "clapfig::Schema requires at least one variant on an enum (an \
             empty enum is uninhabited and cannot be deserialized)",
        ));
    }
    // `#[clapfig(rename_all = ...)]` wins over `#[serde(rename_all = ...)]`
    // when both are present — the clapfig form is the authoritative spelling
    // for what reaches the schema. We still accept the serde form so users
    // don't have to duplicate the attribute.
    let rename_all = struct_attrs
        .rename_all
        .clone()
        .or_else(|| parse_serde_rename_all(type_attrs).ok().flatten());
    let mut out = Vec::with_capacity(data.variants.len());
    let mut seen = std::collections::HashSet::new();
    for variant in &data.variants {
        if !matches!(variant.fields, Fields::Unit) {
            return Err(syn::Error::new(
                variant.fields.span(),
                "clapfig::Schema on enums only supports unit-only variants \
                 (no payload). For variants with payload, use #[clapfig(value)] \
                 on the field and provide your own deserialize.",
            ));
        }
        let name = variant_schema_name(variant, rename_all.as_deref())?;
        if !seen.insert(name.clone()) {
            return Err(syn::Error::new(
                variant.ident.span(),
                format!(
                    "duplicate enum variant name {name:?} after rename — \
                     two variants would produce the same schema value"
                ),
            ));
        }
        out.push(quote! { #name });
    }
    Ok(out)
}

/// Resolve a single variant's schema-facing name: per-variant `rename`
/// wins, otherwise the enum-level `rename_all` applies, otherwise the
/// variant ident verbatim.
fn variant_schema_name(variant: &Variant, rename_all: Option<&str>) -> syn::Result<String> {
    if let Some(name) = parse_variant_rename(&variant.attrs)? {
        return Ok(name);
    }
    let raw = variant.ident.to_string();
    match rename_all {
        Some(rule) => apply_rename_all(&raw, rule).ok_or_else(|| {
            syn::Error::new(
                variant.ident.span(),
                format!(
                    "unsupported rename_all rule {rule:?}; supported: \
                     lowercase, UPPERCASE, PascalCase, camelCase, snake_case, \
                     SCREAMING_SNAKE_CASE, kebab-case, SCREAMING-KEBAB-CASE"
                ),
            )
        }),
        None => Ok(raw),
    }
}

/// Apply a serde-compatible `rename_all` rule to a PascalCase variant
/// name. Returns `None` for unsupported rules so the caller can produce a
/// diagnostic with the offending value.
fn apply_rename_all(name: &str, rule: &str) -> Option<String> {
    match rule {
        "lowercase" => Some(name.to_lowercase()),
        "UPPERCASE" => Some(name.to_uppercase()),
        "PascalCase" => Some(name.to_string()),
        "camelCase" => Some(pascal_to_camel(name)),
        "snake_case" => Some(pascal_to_snake(name, '_')),
        "SCREAMING_SNAKE_CASE" => Some(pascal_to_snake(name, '_').to_uppercase()),
        "kebab-case" => Some(pascal_to_snake(name, '-')),
        "SCREAMING-KEBAB-CASE" => Some(pascal_to_snake(name, '-').to_uppercase()),
        _ => None,
    }
}

/// Convert PascalCase to camelCase, matching serde's `rename_all` behavior:
/// `MyVariant` → `myVariant`, `MyHTTPApi` → `myHttpApi`. Derived from the
/// snake_case form so acronym runs collapse the same way serde does (no
/// internal separators inside an acronym; the first letter of a new word
/// after an acronym keeps the upper-case boundary).
fn pascal_to_camel(name: &str) -> String {
    let snake = pascal_to_snake(name, '_');
    let mut out = String::with_capacity(snake.len());
    let mut next_upper = false;
    for (i, c) in snake.chars().enumerate() {
        if c == '_' {
            next_upper = true;
        } else if i == 0 {
            out.push(c);
        } else if next_upper {
            out.extend(c.to_uppercase());
            next_upper = false;
        } else {
            out.push(c);
        }
    }
    out
}

/// Convert PascalCase to a separator-joined lowercase form, matching the
/// algorithm serde / `heck::AsSnakeCase` use. `sep = '_'` produces
/// snake_case, `sep = '-'` produces kebab-case.
///
/// An uppercase letter inserts the separator *before itself* in two
/// cases — and no other — so acronym runs are kept together:
///   1. it follows a lowercase letter: `MyHttp` → boundary before `H`.
///   2. it follows another uppercase AND is followed by a lowercase
///      letter: in `HTTPApi`, the `A` is the boundary because it starts
///      a new word inside the acronym run; the inner `T`/`T`/`P` keep
///      the previous word's letters together.
///
/// Concretely:
///   `MyVariant`   → `my_variant`
///   `MyHTTPApi`   → `my_http_api`
///   `IOError`     → `io_error`
///   `HTTPServer`  → `http_server`
///
/// (Verified against serde's rename_all in upstream issues; reproducing
/// the algorithm here lets `clapfig-derive` avoid adding a `heck`
/// dependency for a single text transform.)
fn pascal_to_snake(name: &str, sep: char) -> String {
    let chars: Vec<char> = name.chars().collect();
    let mut out = String::with_capacity(name.len() + 4);
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() && i > 0 {
            let prev = chars[i - 1];
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            // Boundary 1: lowercase → upper.
            // Boundary 2: upper → upper, followed by a lowercase letter
            // (i.e. the current upper starts a new word inside an
            // acronym-then-PascalCase sequence).
            if prev.is_lowercase() || (prev.is_uppercase() && next_lower) {
                out.push(sep);
            }
        }
        if c.is_uppercase() {
            out.extend(c.to_lowercase());
        } else {
            out.push(c);
        }
    }
    out
}

/// Parse `#[clapfig(rename = "name")]` or `#[serde(rename = "name")]` off a
/// variant. Returns the override string or `None` if neither is present.
fn parse_variant_rename(attrs: &[Attribute]) -> syn::Result<Option<String>> {
    let mut found: Option<String> = None;
    for attr in attrs {
        let path = attr.path();
        let is_clapfig = path.is_ident("clapfig");
        let is_serde = path.is_ident("serde");
        if !is_clapfig && !is_serde {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                let value: syn::LitStr = meta.value()?.parse()?;
                // clapfig wins over serde if both are present on the same
                // variant. We accept serde for migration convenience; the
                // clapfig form is the authoritative spelling.
                if is_clapfig || found.is_none() {
                    found = Some(value.value());
                }
                Ok(())
            } else {
                // Silently skip other meta items — serde has many unrelated
                // attrs (`alias`, `borrow`, etc.) we mustn't choke on.
                let _ = meta.input.parse::<proc_macro2::TokenStream>();
                Ok(())
            }
        })?;
    }
    Ok(found)
}

/// Parse `#[serde(rename_all = "...")]` off the enum's attrs as a fallback
/// when no `#[clapfig(rename_all = ...)]` was given.
fn parse_serde_rename_all(attrs: &[Attribute]) -> syn::Result<Option<String>> {
    let mut found: Option<String> = None;
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename_all") {
                let value: syn::LitStr = meta.value()?.parse()?;
                if found.is_none() {
                    found = Some(value.value());
                }
                Ok(())
            } else {
                let _ = meta.input.parse::<proc_macro2::TokenStream>();
                Ok(())
            }
        })?;
    }
    Ok(found)
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
    /// `#[clapfig(rename_all = "...")]` — applies to every variant of a
    /// unit-only enum that derives `Schema`. Unused on structs (an
    /// equivalent rename for struct fields would conflict with serde's
    /// existing `#[serde(rename_all)]` rule, which we leave authoritative
    /// for deserialize.)
    rename_all: Option<String>,
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
            } else if meta.path.is_ident("rename_all") {
                let value: syn::LitStr = meta.value()?.parse()?;
                out.rename_all = Some(value.value());
                Ok(())
            } else {
                Err(meta.error(format!(
                    "unsupported #[clapfig(...)] type attribute: `{}`. \
                     Supported: name = \"...\", strict = true/false, \
                     rename_all = \"...\"",
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
    /// `HashMap<String, V>` / `BTreeMap<String, V>` where V is a leaf shape —
    /// emits `LeafType::Map(V)`. TOML map keys must be strings.
    Map(TokenStream2),
    /// `HashMap<String, NestedStruct>` / `BTreeMap<String, NestedStruct>` —
    /// emits `FieldStatic::MapOf(<NestedStruct as Schema>::STATIC)`. The
    /// inner token is the same `<T as Schema>::STATIC` reference produced
    /// for plain nested fields; the converter routes it into a `Field::MapOf`
    /// at the runtime layer.
    MapOfNested(TokenStream2),
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
        // `Option<Option<T>>` is almost always a user error — the outer
        // Option's `None` and the inner Option's `None` collapse to the
        // same observable state at the schema layer. Catch it cleanly
        // here instead of accepting a redundant optional flag.
        if matches!(inner_shape, TypeShape::Optional(_)) {
            return Err(syn::Error::new(
                inner.span(),
                "Option<Option<T>> is not supported — collapse to a single Option<T>. \
                 If you need to distinguish 'absent from config' from 'explicitly set to \
                 a null-like value', use a `#[clapfig(value)]` field with a typed enum.",
            ));
        }
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
            TypeShape::Map(_) | TypeShape::MapOfNested(_) => Err(syn::Error::new(
                inner.span(),
                "Vec<HashMap<...>> / Vec<BTreeMap<...>> is not supported by clapfig::Schema. \
                 Use `#[clapfig(value)]` with `toml::Value` for free-form nested shapes.",
            )),
        };
    }
    if name == "HashMap" || name == "BTreeMap" {
        let (key_ty, value_ty) = two_generic_arguments(&last.arguments, &name)?;
        // TOML map keys are strings — `LeafType::Map(V)` has no key-type
        // discriminant on the value level. Reject any non-String key at
        // derive time with a clear message instead of letting the schema
        // emit something the deserializer can't satisfy.
        if !is_string_path(key_ty) {
            return Err(syn::Error::new(
                key_ty.span(),
                format!(
                    "{name}<K, V> requires `K = String` (TOML map keys are string-typed); \
                     numeric or enum keys aren't representable. Store the key inside the value type."
                ),
            ));
        }
        let value_shape = classify_type(value_ty)?;
        return match value_shape {
            TypeShape::Scalar(_, tok) => Ok(TypeShape::Map(tok)),
            TypeShape::Value => Ok(TypeShape::Map(
                quote! { ::clapfig::static_schema::LeafTypeStatic::Value },
            )),
            TypeShape::Array(elem) => Ok(TypeShape::Map(
                quote! { ::clapfig::static_schema::LeafTypeStatic::Array(&#elem) },
            )),
            TypeShape::Optional(_) => Err(syn::Error::new(
                value_ty.span(),
                format!(
                    "{name}<String, Option<T>> is not supported — an absent map entry is \
                     already 'optional'; omit the Option<T> wrapper."
                ),
            )),
            TypeShape::Map(_) | TypeShape::MapOfNested(_) => Err(syn::Error::new(
                value_ty.span(),
                format!(
                    "{name}<String, {name}<...>> (map-of-map) is not yet supported by clapfig::Schema. \
                     Use `#[clapfig(value)]` with `toml::Value` for free-form nested shapes."
                ),
            )),
            // {Hash,BTree}Map<String, NestedStruct> → `FieldStatic::MapOf` at the
            // runtime layer. The inner expression is the same `<T as Schema>::STATIC`
            // we use for plain Nested fields; the converter sees a `MapOf` and
            // emits `Field::MapOf(schema)`.
            TypeShape::Nested(inner_expr) => Ok(TypeShape::MapOfNested(inner_expr)),
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

fn two_generic_arguments<'a>(
    args: &'a PathArguments,
    parent: &str,
) -> syn::Result<(&'a Type, &'a Type)> {
    let abga = match args {
        PathArguments::AngleBracketed(a) => a,
        _ => {
            return Err(syn::Error::new(
                args.span(),
                format!("{parent} requires two type arguments (K, V)"),
            ));
        }
    };
    if abga.args.len() != 2 {
        return Err(syn::Error::new(
            abga.span(),
            format!("{parent} requires exactly two type arguments (K, V)"),
        ));
    }
    let mut iter = abga.args.iter();
    let k = match iter.next().unwrap() {
        GenericArgument::Type(t) => t,
        other => {
            return Err(syn::Error::new(
                other.span(),
                format!("{parent}'s key argument must be a type"),
            ));
        }
    };
    let v = match iter.next().unwrap() {
        GenericArgument::Type(t) => t,
        other => {
            return Err(syn::Error::new(
                other.span(),
                format!("{parent}'s value argument must be a type"),
            ));
        }
    };
    Ok((k, v))
}

/// Syntactic accessor for the inner type of an outer-most `Option<...>`.
/// Returns `None` for non-Option types. Used by the `value`-fast-path's
/// `Option<Option<T>>` rejection — we need the inner type to re-check,
/// but must not recurse through `classify_type` (the whole point of the
/// fast path is to skip it).
fn outer_option_inner_type(ty: &Type) -> Option<&Type> {
    if let Type::Path(TypePath { path, qself: None }) = ty
        && let Some(last) = path.segments.last()
        && last.ident == "Option"
        && let PathArguments::AngleBracketed(args) = &last.arguments
        && let Some(GenericArgument::Type(inner)) = args.args.first()
    {
        return Some(inner);
    }
    None
}

/// Syntactic check for whether the outer-most type is `Option<...>`.
///
/// Used by the `#[clapfig(value)]` escape-hatch path so we can preserve
/// the field's optionality without recursing into the inner type — `value`
/// explicitly bypasses shape inference, so we must not run
/// `classify_type` on the inner. Accepts any path whose last segment is
/// `Option` (works for `Option<T>`, `std::option::Option<T>`, etc.).
fn is_outer_option_type(ty: &Type) -> bool {
    if let Type::Path(TypePath { path, qself: None }) = ty
        && let Some(last) = path.segments.last()
    {
        return last.ident == "Option";
    }
    false
}

/// Last-segment check for `String` (or qualified `std::string::String`).
fn is_string_path(ty: &Type) -> bool {
    if let Type::Path(TypePath { path, qself: None }) = ty
        && let Some(last) = path.segments.last()
    {
        return last.ident == "String" && last.arguments.is_empty();
    }
    false
}

fn is_toml_value_path(path: &syn::Path) -> bool {
    // Strict suffix match for the toml crate's `Value` type:
    //   `Value`                     — assumed to be a use-imported toml::Value
    //   `toml::Value`               — canonical form (incl. leading `::`)
    // Anything else (e.g. `my_crate::Value`, `crate::toml::Value`, or a
    // longer path ending in `toml::Value`) is rejected. The leading-colon
    // form parses as the same segments — `syn::Path::leading_colon` is a
    // separate field, not a segment.
    let segs: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    matches!(segs.as_slice(),
        [a] if a == "Value"
    ) || matches!(segs.as_slice(),
        [a, b] if a == "toml" && b == "Value"
    )
}

fn is_toml_datetime_path(path: &syn::Path) -> bool {
    // Strict suffix match for `toml::value::Datetime`:
    //   `Datetime`                  — use-imported
    //   `toml::Datetime`            — common re-export
    //   `toml::value::Datetime`     — canonical
    // Any other path is rejected — `my_crate::toml::internal::value::Datetime`
    // and friends do NOT match.
    let segs: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    matches!(segs.as_slice(),
        [a] if a == "Datetime"
    ) || matches!(segs.as_slice(),
        [a, b] if a == "toml" && b == "Datetime"
    ) || matches!(segs.as_slice(),
        [a, b, c] if a == "toml" && b == "value" && c == "Datetime"
    )
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

    // `#[clapfig(value)]` is the universal escape hatch: the user opts out
    // of shape inference and takes responsibility for the deserialize side
    // (typically a `#[serde(untagged)]` enum, a custom Rust enum, or any
    // other type clapfig wouldn't otherwise recognize as a leaf). Skip
    // `classify_type` entirely — running it would either route through the
    // Nested-branch rejection below (for custom Pascal-case types) or
    // through `Map<String, NestedStruct>` rejection (for maps of custom
    // values), neither of which fits the override's documented contract.
    // We only need the outer-Option signal so `optional_from_type` is
    // correct.
    let shape = if attrs.force_value {
        // Even on the value-fast-path, `Option<Option<T>>` remains a
        // user error — the inner None and outer None collapse to the
        // same observable state regardless of whether we ran shape
        // inference on the inner type. Detect at the syntactic level
        // (no inner `classify_type` recursion, which is the whole point
        // of the fast path).
        if let Some(inner) = outer_option_inner_type(&field.ty)
            && is_outer_option_type(inner)
        {
            return Err(syn::Error::new(
                field.ty.span(),
                "Option<Option<T>> is not supported even with #[clapfig(value)] — \
                 collapse to a single Option<T>; the inner Option's None is \
                 indistinguishable at the schema layer.",
            ));
        }
        if is_outer_option_type(&field.ty) {
            TypeShape::Optional(Box::new(TypeShape::Value))
        } else {
            TypeShape::Value
        }
    } else {
        classify_type(&field.ty)?
    };

    // Nested struct OR unit-only enum field. The macro can't tell the
    // two apart syntactically — so the routing depends on what
    // attributes the user wrote and whether `Option<…>` is in the way:
    //
    //   1. Bare nested type, no leaf attrs (`db: DbConfig`,
    //      `page_size: PdfPageSize`) → emit `FieldStatic::Nested`. The
    //      converter inspects `enum_variants` and flattens enum-kind to
    //      `Field::Leaf(LeafType::Enum)` automatically.
    //
    //   2. Nested type with leaf attrs OR wrapped in `Option<…>`
    //      (`#[clapfig(default = "lexed")] page_size: PdfPageSize`,
    //      `mode: Option<Mode>`) → emit `FieldStatic::Leaf` carrying
    //      `LeafTypeStatic::EnumRef(<T as Schema>::STATIC)`. The
    //      converter checks `is_enum()` at first `schema()` call and
    //      either emits `LeafType::Enum` with the attrs applied OR
    //      panics with a clear authoring-error message pointing at the
    //      field (same deferred-error pattern as malformed datetime
    //      defaults). `allowed` stays mutually exclusive with the
    //      enum-of-variants set the type already declares.
    let nested_inner_expr: Option<&TokenStream2> = match &shape {
        TypeShape::Nested(inner) => Some(inner),
        TypeShape::Optional(inner) => match inner.as_ref() {
            TypeShape::Nested(expr) => Some(expr),
            _ => None,
        },
        _ => None,
    };
    if let Some(inner_expr) = nested_inner_expr {
        // `Option<T>` at the field type carries the same "leaf may be
        // absent" signal as an explicit `#[clapfig(optional)]` attr —
        // fold them both into one flag so the rest of the path checks a
        // single condition.
        let is_field_optional = attrs.optional || matches!(&shape, TypeShape::Optional(_));
        let has_leaf_attrs = attrs.default.is_some() || attrs.env.is_some() || is_field_optional;
        if attrs.allowed.is_some() {
            return Err(syn::Error::new(
                field.span(),
                "`#[clapfig(allowed = [...])]` is not valid on a \
                 nested-schema field — the inner type's `Schema` impl already \
                 declares the value set. For enum-typed fields, drop \
                 `allowed`; for struct-typed fields, drop the whole attribute.",
            ));
        }
        if has_leaf_attrs {
            let default_expr = match &attrs.default {
                Some(expr) => {
                    // EnumRef leaves take a string-shaped default (variant
                    // name). Other shapes would require knowing the
                    // referenced schema's variant types at macro time,
                    // which we don't have access to — string keeps the
                    // common case (user names a variant) and the converter
                    // will type-check it against the actual variant set.
                    let v = expr_to_value_static(
                        expr,
                        &TypeShape::Scalar(
                            ScalarKind::String,
                            quote! { ::clapfig::static_schema::LeafTypeStatic::String },
                        ),
                    )?;
                    quote! { Some(#v) }
                }
                None => quote! { None },
            };
            let env_expr = match &attrs.env {
                Some(s) => quote! { Some(#s) },
                None => quote! { None },
            };
            let optional_expr = quote! { #is_field_optional };
            let leaf = quote! {
                ::clapfig::static_schema::LeafStatic {
                    doc: #doc_expr,
                    ty: ::clapfig::static_schema::LeafTypeStatic::EnumRef {
                        schema: #inner_expr,
                        field_name: #name,
                    },
                    default: #default_expr,
                    optional: #optional_expr,
                    env: #env_expr,
                }
            };
            return Ok(quote! {
                ::clapfig::static_schema::NamedFieldStatic {
                    name: #name,
                    field: ::clapfig::static_schema::FieldStatic::Leaf(#leaf),
                }
            });
        }
        // Bare nested with no leaf attrs — original path.
        return Ok(quote! {
            ::clapfig::static_schema::NamedFieldStatic {
                name: #name,
                field: ::clapfig::static_schema::FieldStatic::Nested(#inner_expr),
            }
        });
    }

    // `{Hash,BTree}Map<String, NestedStruct>` → FieldStatic::MapOf. The
    // runtime side has no place to attach a `default` / `env` /
    // `optional` to a map of user-keyed nested objects, so leaf attrs
    // here remain a hard error.
    if let TypeShape::MapOfNested(inner_expr) = &shape {
        if attrs.default.is_some()
            || attrs.env.is_some()
            || attrs.allowed.is_some()
            || attrs.optional
        {
            return Err(syn::Error::new(
                field.span(),
                "leaf attributes (default, env, allowed, optional) are not \
                 valid on map-of-nested-struct fields — entry presence is \
                 already user-controlled, and a single per-field default \
                 has no meaning across an arbitrary set of entry keys.",
            ));
        }
        return Ok(quote! {
            ::clapfig::static_schema::NamedFieldStatic {
                name: #name,
                field: ::clapfig::static_schema::FieldStatic::MapOf(#inner_expr),
            }
        });
    }

    // `Option<{Hash,BTree}Map<String, NestedStruct>>` — no representation:
    // an absent MapOf is already the empty map (the natural optional
    // state), so wrapping in Option adds no signal and there's no
    // `FieldStatic` shape to encode it. Keep the explicit diagnostic.
    if let TypeShape::Optional(inner) = &shape
        && matches!(inner.as_ref(), TypeShape::MapOfNested(_))
    {
        return Err(syn::Error::new(
            field.ty.span(),
            "Option<Map<String, NestedStruct>> is not supported by \
             clapfig::Schema — an absent map is already the empty map. Drop \
             the `Option` wrapper and use the bare map type.",
        ));
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
        TypeShape::Array(_)
        | TypeShape::Map(_)
        | TypeShape::MapOfNested(_)
        | TypeShape::Value
        | TypeShape::Nested(_) => false,
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
        TypeShape::Map(val) => Ok((
            quote! { ::clapfig::static_schema::LeafTypeStatic::Map(&#val) },
            false,
        )),
        TypeShape::Value => Ok((
            quote! { ::clapfig::static_schema::LeafTypeStatic::Value },
            false,
        )),
        TypeShape::Nested(_) | TypeShape::MapOfNested(_) => {
            unreachable!("nested / map-of-nested handled before leaf-type dispatch")
        }
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
