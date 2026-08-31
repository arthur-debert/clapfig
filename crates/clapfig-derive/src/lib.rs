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
    PathArguments, Type, TypePath, Variant, ext::IdentExt, parse_macro_input, spanned::Spanned,
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
///   mapped to TOML's signed 64-bit integer, carrying the source width's
///   bounds so out-of-range values fail schema validation with the key
///   path; see the `LeafTypeStatic::Integer` doc comment for the
///   `i64::MAX` caveat on the unsigned variants), `f32`, `f64`,
///   `clapfig::value::Datetime`, `clapfig::value::Value`.
/// - `Option<T>` of a supported leaf: scalar, `Value`, or unit-only enum
///   (`Option<UnitEnum>` included). Unqualified `Option<T>` is not the
///   contract — nested structs are not an Option shape (see deferred
///   panics below).
/// - `Vec<T>` where `T` is a scalar: runtime `Shape::Array` of that leaf
///   (derive emits `LeafTypeStatic::Array`).
/// - `Vec<T>` where `T` derives `clapfig::Schema`: runtime `Shape::Array`
///   of the item (TOML `[[name]]` array of tables) for a struct element
///   type; a unit-only-enum element type is `Shape::Array` of an enum
///   leaf (allowed-values on items). Derive emits `FieldStatic::ArrayOf`.
///   Support is trait-resolved — the compiler, not a syntactic guess,
///   decides whether `T` qualifies. An absent array loads as the empty
///   `Vec`; `Option<Vec<T>>` of a scalar or unit-only enum keeps the
///   presence signal (absent → `None`). `Option<Vec<NestedStruct>>` has
///   no representation (an absent array of nested objects is already the
///   empty array — spell it `Vec<T>`) and panics at the first
///   `schema()` call.
/// - `HashMap<String, V>` / `BTreeMap<String, V>`: a scalar / `Value` /
///   array-of-scalar `V` is runtime `Shape::Map` of that leaf; a nested
///   struct `V` is `Shape::Map` of that object; a unit-only-enum `V` is
///   `Shape::Map` of an enum leaf. Derive emits `LeafTypeStatic::Map` or
///   `FieldStatic::MapOf`; conversion collapses both to `Shape::Map`.
///   An absent map loads as the empty map. As the **document root**,
///   `Clapfig::typed::<BTreeMap<String, T>>()` / `HashMap<String, T>`
///   where `T: Schema` loads without a parent field.
///   `Option<HashMap<String, V>>` / `Option<BTreeMap<String, V>>` keeps
///   the presence signal when `V` is a scalar / `Value` /
///   array-of-scalar. `Option` around a map of objects
///   (`Option<HashMap<String, NestedStruct>>`, including a unit-enum `V`
///   routed through the same Nested classification) is a derive-time
///   error.
/// - Nested struct: assumed to also derive `clapfig::Schema`; produces a
///   `FieldStatic::Nested { .. }`.
/// - Unit-only enum: flattens at every use site to a `LeafType::Enum`
///   leaf (or `Shape::Map` / `Shape::Array` of that leaf in the
///   map/array positions above). Variant names are the allowed values;
///   `rename` / `rename_all` apply to those spellings.
/// - Internally tagged enum (`#[serde(tag = "...")]`): emits
///   `Shape::Tagged`. Unit variants are the empty object; named-field
///   struct variants are that object. The tag is reserved (not a field
///   of any variant). Discriminators follow the same `rename` /
///   `rename_all` rules as unit-only enums. A legal document root.
///
/// Field types named `Datetime` / `Value` are matched *by name* (a proc
/// macro cannot resolve paths), so the macro also emits a compile-time
/// assertion that the type really is clapfig's — a user-defined lookalike
/// (`struct Datetime`) is a compile error instead of a silently mis-typed
/// leaf. Raw identifiers unraw: a field `r#type` emits the schema name
/// `type`, matching serde's spelling.
///
/// # Rejected at derive time
///
/// - `i128` / `u128` (TOML's integer width is signed 64-bit).
/// - Generic types and `where` clauses (`static SchemaStatic` cannot
///   reference type parameters).
/// - Tuple structs, unit structs, tuple/newtype enum variants, and
///   struct-variant enums that are not internally tagged
///   (`#[serde(tag = "...")]`).
/// - `Option<Option<T>>`.
/// - Non-`String` map keys; `HashMap`/`BTreeMap` of `Option<T>`, of
///   another map, or of `Vec<NestedStruct>`.
/// - `Vec<Option<T>>`, `Vec<Vec<...>>`, `Vec<clapfig::value::Value>`,
///   `Vec<HashMap<...>>` / `Vec<BTreeMap<...>>`.
/// - `PathBuf`, `Duration`, `char`, newtypes, type aliases, third-party
///   maps — no TOML-faithful schema shape; the `Schema` trait's
///   `on_unimplemented` diagnostic names the supported set and the
///   `#[clapfig(value)]` escape hatch.
/// - Datetime / Value lookalikes claimed by type name that are not
///   clapfig's own types.
/// - Unknown `#[clapfig(...)]` metas (fields, structs, enum variants).
/// - `#[clapfig(name)]` / `#[clapfig(strict)]` on unit-only enums
///   (flattened away). Internally tagged enums accept both.
/// - Kind-mismatched `default` / `allowed` literals; empty `allowed`;
///   `value` + `allowed` on the same field; `allowed` on `Vec` /
///   nested / map-of-nested fields; defaults on map-typed fields and
///   array-of-nested fields; leaf attrs (`default` / `env` / `allowed` /
///   `optional`) on map-of-nested and array-of-nested fields;
///   `Option<HashMap<String, NestedStruct>>` /
///   `Option<BTreeMap<String, NestedStruct>>` (unit-enum values
///   included — they classify as Nested at the field site).
/// - Invalid or colliding rename strings; clapfig/serde rename (or
///   `rename_all`) pairs that disagree; unsupported `rename_all` rules.
/// - Every serde attribute the schema does not honor (see *Serde
///   attributes*).
///
/// # Deferred panics at the first `schema()` call
///
/// Authoring errors the macro cannot catch at derive time (enum-vs-struct
/// kind, datetime parsing). Same first-`schema()`-call failure mode as
/// each other:
///
/// - `Option<NestedStruct>` — drop the `Option`; an absent nested
///   section is already the empty-table state.
/// - `Option<Vec<NestedStruct>>` — drop the `Option`; an absent array
///   of nested objects is already the empty array.
/// - Leaf attributes (`default` / `env` / `optional`) on a struct-typed
///   nested field — drop the attributes; struct fields are nested-section
///   shaped. (`allowed` on a nested field is a derive-time error.)
/// - A default on an enum-typed field that is not a variant (post-rename
///   spelling) — see *Field attributes*.
/// - A malformed datetime default literal — see the datetime caveat
///   under *Field attributes*.
///
/// # Field attributes
///
/// - `#[clapfig(default = <literal>)]` — scalar default. Accepts string,
///   integer, float, bool, and unary-negated numeric literals
///   (`-9223372036854775808i64` works for `i64::MIN`); on `Vec<T>` fields,
///   also accepts an array literal of literals. On
///   `clapfig::value::Datetime` fields, a string literal is emitted as
///   `ValueStatic::Datetime`. Default literals are kind-checked against
///   the field's TOML type at derive time (per element for array
///   literals), the same way `allowed` literals are; a default outside the
///   field's `allowed = [...]` set is likewise a derive error. Defaults on
///   enum-typed fields are checked against the variant set at the first
///   `schema()` call (the variant list lives on another type the macro
///   can't see). Map-typed fields and array-of-nested-schema fields do
///   not accept defaults (entries are user-supplied).
///
///   **Datetime caveat:** datetime defaults are *not* parsed at derive
///   time — the macro intentionally avoids pulling a datetime parser
///   into its dependency tree. A malformed datetime literal (e.g.
///   `default = "not-a-date"` on a `Datetime` field) compiles
///   successfully and panics with `"clapfig: invalid datetime literal
///   in static schema default"` the first time `Schema::schema()` is
///   called (typically at app startup). Verify your datetime defaults
///   match TOML's grammar (RFC 3339 offset / local datetime / local
///   date / local time) before shipping.
/// - `#[clapfig(env = "NAME")]` — explicit env-var override
/// - `#[clapfig(rename = "name")]` — override the field's schema name.
///   `#[serde(rename = "name")]` alone also works — the schema follows
///   serde's spelling so the merged config and the typed deserialize agree.
///   The directional `#[serde(rename(deserialize = "...", ...))]` form
///   contributes its `deserialize` spelling (a serialize-only rename
///   leaves the schema on the Rust identifier, matching serde). If both
///   attributes are present they must match (a differing pair is a
///   derive-time error), and a `#[clapfig(rename)]` without the matching
///   serde attribute still needs one for the deserialize side. An explicit
///   rename exempts the field from a struct-level `rename_all` rule (see
///   *Struct attributes*). Rename strings are validated at derive time
///   (non-empty, no `.`, `[` or `]` — the runtime builder's field-name
///   rules), and two fields resolving to the same schema name (after
///   renames) are a derive error.
/// - `#[clapfig(value)]` — force `LeafType::Value` (untyped escape hatch
///   — meant for fields whose value can take multiple incompatible
///   shapes, e.g. a `#[serde(untagged)] enum`. The macro does not
///   constrain which field type this is applied to: the caller takes
///   responsibility for the deserialize side)
/// - `#[clapfig(allowed = [...])]` — set `LeafType::Enum` on a scalar
///   leaf. Works on `String`, integer, float, and `bool` fields; each
///   listed literal must match the field's TOML type, and at least one
///   value is required. Negative integer/float literals are accepted.
/// - `#[clapfig(min = <integer>)]` / `#[clapfig(max = <integer>)]` —
///   tighten an integer field's accepted range. The declared range is
///   intersected with the Rust integer type's range (`u8` still caps at
///   `255`), contradictory bounds are derive-time errors, and integer
///   defaults must fall inside the resulting range.
/// - `#[clapfig(optional)]` — force `optional = true` on a non-`Option<T>`
///   field (rarely needed; `Option<T>` is the usual spelling)
///
/// # Struct attributes
///
/// - `#[clapfig(name = "Name")]` — override the schema's name (default:
///   struct name)
/// - `#[clapfig(strict = true/false)]` — set per-node strictness for the
///   cascade
/// - `#[clapfig(rename_all = "rule")]` — rewrite every field name that has
///   no explicit rename through a serde-compatible rule (`lowercase`,
///   `UPPERCASE`, `PascalCase`, `camelCase`, `snake_case`,
///   `SCREAMING_SNAKE_CASE`, `kebab-case`, `SCREAMING-KEBAB-CASE`; any
///   other rule is a derive-time error). Like `#[clapfig(rename)]`, the
///   clapfig spelling converts the *schema only* — the macro cannot
///   change serde's separately generated `Deserialize` impl, so on a
///   typed struct the clapfig spelling alone makes validation accept
///   converted keys that serde's deserialize then fails to find. For
///   typed configurations use `#[serde(rename_all = "rule")]` (or pair
///   both attributes with the same rule); the clapfig spelling on its
///   own is for schema-only types that don't derive `Deserialize`.
///   Serde's directional `rename_all(deserialize = "rule", ...)` form
///   contributes its `deserialize` rule (a serialize-only
///   `rename_all(serialize = ...)` is accepted and leaves the schema on
///   the Rust identifiers, matching serde). If both spellings are
///   present they must name the same rule — a differing pair is a
///   derive-time error. The conversion is serde-exact (`serde_derive`'s
///   `RenameRule::apply_to_field`), so the schema's names are identical
///   to serde's converted deserialize names;
///   explicit field renames win over the rule, and the duplicate-name /
///   rename-conflict diagnostics apply to the converted names. Note that
///   a `kebab-case` schema is incompatible with the builder's
///   `normalize_keys(true)` mode, which canonicalizes incoming keys to
///   snake_case — kebab schema names would never match; use one or the
///   other.
///
/// Both are rejected on unit-only enums: the enum flattens to a
/// value-level `LeafType::Enum` at every use site, which discards them.
/// Internally tagged enums keep `name` / `strict` (they are a real schema
/// node). On enum *variants* the only supported clapfig attribute is
/// `rename = "..."`; anything else is a derive error (same strictness as
/// fields and types).
///
/// # Serde attributes (reject-loudly policy)
///
/// Any `#[serde(...)]` attribute the derived schema does not honor is a
/// **derive-time error** naming the attribute and the divergence it would
/// cause — never silently ignored. Honored: `rename` (fields/variants,
/// directional included), `rename_all` (structs, unit-only enums, and
/// internally tagged enums, directional included — see *Struct
/// attributes*), `#[serde(tag = "...")]` on enums (internally tagged
/// unions), and `deserialize_with`/`with` (the typed deserialize runs
/// through serde, so custom deserializers apply to every source).
/// `deserialize_with` is honored for *shape-preserving* normalization:
/// the schema keeps advertising the field's inferred shape and validates
/// the merged map against it *before* serde runs, so a deserializer
/// expecting a different wire shape gets its inputs rejected by schema
/// validation with a loud type error — a load failure, never a silently
/// mis-typed value. Pair a shape-changing deserializer with
/// `#[clapfig(value)]` so the schema declares a free-form leaf and steps
/// aside. Serialize-only attributes (`skip_serializing`, `serialize_with`,
/// …) and derive plumbing (`bound`, `borrow`, `crate`, `expecting`) are
/// accepted — they cannot make the schema disagree with serde's
/// deserialize. Everything else (`default`, `flatten`, `alias`, `skip`,
/// `skip_deserializing`, `untagged`/`content` (adjacent tagging),
/// `deny_unknown_fields`, `transparent`, `from`/`try_from`, …) is
/// rejected. There is no clapfig-only `#[clapfig(tag)]`.
///
/// `rename_all` (clapfig or serde spelling, including serde's directional
/// `rename_all(deserialize = "...")` form) rewrites field names on structs
/// and variant names on unit-only enums. The serde spelling (or a
/// matching pair) keeps the schema identical to what serde's deserialize
/// expects; the clapfig spelling alone converts only the schema. See
/// *Struct attributes* for the rule set and precedence.
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
             `Clapfig::builder(Schema::object(...))`.",
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
        .unwrap_or_else(|| type_name.unraw().to_string());
    let type_doc = collect_doc_lines(&input.attrs);

    // Collected compile-time assertions for type paths the macro claims by
    // name (`Datetime` / `Value`); emitted alongside the schema statics.
    let mut claim_asserts: Vec<TokenStream2> = Vec::new();

    let mut is_document_root = false;
    let mut extra_statics: Vec<TokenStream2> = Vec::new();
    let mut tagged_tag_body = quote! { "" };
    let mut tagged_variants_body = quote! { &[] };
    let (fields_body, enum_variants_body) = match &input.data {
        Data::Struct(s) => match &s.fields {
            Fields::Named(named) => {
                // Struct-level `rename_all` (either spelling, serde's
                // directional `rename_all(deserialize = "...")` included):
                // schema field names follow serde's converted spelling, so
                // with the serde attribute (or a matching pair) the schema
                // and serde's deserialize agree on one set of names. The
                // clapfig spelling alone converts only the schema — the
                // macro can't touch serde's generated `Deserialize`. A
                // differing clapfig/serde pair is a hard error — same
                // contract as field-level renames.
                let serde_rename_all = find_serde_string_meta(&input.attrs, "rename_all");
                let rename_all = resolve_rename_all_rule(
                    struct_attrs.rename_all.as_ref(),
                    serde_rename_all,
                    input.ident.span(),
                    "field names",
                )?;
                if let Some(rule) = &rename_all
                    && !is_supported_rename_all_rule(rule)
                {
                    return Err(syn::Error::new(
                        input.ident.span(),
                        format!(
                            "unsupported rename_all rule {rule:?}; supported: \
                             lowercase, UPPERCASE, PascalCase, camelCase, snake_case, \
                             SCREAMING_SNAKE_CASE, kebab-case, SCREAMING-KEBAB-CASE"
                        ),
                    ));
                }
                check_serde_attrs(&input.attrs, SerdeCtx::Struct)?;
                let mut field_entries = Vec::with_capacity(named.named.len());
                // Post-rename duplicate detection: the enum path already
                // has one (two variants renamed onto the same value are a
                // derive error); the struct path needs the same guard or
                // `find_field` lookups and unknown-key validation become
                // order-dependent at runtime.
                let mut seen = std::collections::HashSet::new();
                for f in &named.named {
                    let expanded = expand_field(f, rename_all.as_deref())?;
                    if !seen.insert(expanded.name.clone()) {
                        return Err(syn::Error::new(
                            f.ident.span(),
                            format!(
                                "duplicate schema field name {:?} — two fields (after \
                                 `rename`/`rename_all`) would collide in the schema, \
                                 making lookups and unknown-key validation \
                                 order-dependent. Rename one of them.",
                                expanded.name
                            ),
                        ));
                    }
                    claim_asserts.extend(expanded.claim_asserts);
                    field_entries.push(expanded.entry);
                }
                is_document_root = true;
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
            if serde_has_meta(&input.attrs, "untagged") {
                // Reject at the `untagged` token (UnitEnum does not honor it).
                check_serde_attrs(&input.attrs, SerdeCtx::UnitEnum)?;
            }
            if serde_has_meta(&input.attrs, "content") {
                // Adjacent tagging: `tag` is honored, `content` is not.
                check_serde_attrs(&input.attrs, SerdeCtx::TaggedEnum)?;
            }
            if serde_has_meta(&input.attrs, "tag") {
                let Some(tag) = find_serde_string_meta(&input.attrs, "tag") else {
                    return Err(syn::Error::new(
                        input.ident.span(),
                        "#[serde(tag)] requires a field name string, e.g. #[serde(tag = \"kind\")]",
                    ));
                };
                validate_schema_field_name(&tag, "#[serde(tag)]", input.ident.span())?;
                check_serde_attrs(&input.attrs, SerdeCtx::TaggedEnum)?;
                let expanded = expand_tagged_enum(
                    type_name,
                    &input.attrs,
                    &struct_attrs,
                    e,
                    &tag,
                    input.ident.span(),
                )?;
                extra_statics = expanded.extra_statics;
                claim_asserts.extend(expanded.claim_asserts);
                tagged_tag_body = quote! { #tag };
                tagged_variants_body = expanded.variants_tokens;
                is_document_root = true;
                (quote! { &[] }, quote! { &[] })
            } else {
                // A unit-only enum flattens to a value-level `LeafType::Enum`
                // at every field that uses it — the flatten reads only the
                // variant list (and doc), so `name` / `strict` would be
                // accepted and then silently discarded. Reject instead.
                if struct_attrs.name.is_some() {
                    return Err(syn::Error::new(
                        input.ident.span(),
                        "#[clapfig(name = \"...\")] has no effect on a unit-only enum — the \
                         enum flattens to a value-level `LeafType::Enum` at each use site and \
                         the schema name is discarded there. Remove the attribute.",
                    ));
                }
                if struct_attrs.strict.is_some() {
                    return Err(syn::Error::new(
                        input.ident.span(),
                        "#[clapfig(strict = ...)] has no effect on a unit-only enum — \
                         strictness governs nested-section schemas, and the enum flattens to \
                         a leaf whose flag is discarded. Remove the attribute (set strictness \
                         on the struct that owns the field instead).",
                    ));
                }
                check_serde_attrs(&input.attrs, SerdeCtx::UnitEnum)?;
                let variants =
                    expand_enum_variants(&input.attrs, &struct_attrs, e, input.ident.span())?;
                (quote! { &[] }, quote! { &[ #(#variants),* ] })
            }
        }
        other => {
            return Err(syn::Error::new(
                input.ident.span(),
                format!(
                    "clapfig::Schema can only be derived for structs, unit-only enums, and internally tagged enums (not {:?})",
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
    let shape_cache_ident = quote::format_ident!("__CLAPFIG_SHAPE_{}", type_name);
    let document_root_impl = if is_document_root {
        quote! { impl ::clapfig::DocumentRoot for #type_name {} }
    } else {
        quote! {}
    };

    let output = quote! {
        #(#claim_asserts)*
        #(#extra_statics)*

        #[allow(non_upper_case_globals)]
        static #static_ident: ::clapfig::static_schema::SchemaStatic =
            ::clapfig::static_schema::SchemaStatic {
                name: #schema_name,
                doc: #doc_expr,
                strict: #strict_expr,
                fields: #fields_body,
                enum_variants: #enum_variants_body,
                tagged_tag: #tagged_tag_body,
                tagged_variants: #tagged_variants_body,
            };

        #[allow(non_upper_case_globals)]
        static #cache_ident: ::std::sync::OnceLock<
            ::std::sync::Arc<::clapfig::runtime::Schema>,
        > = ::std::sync::OnceLock::new();

        #[allow(non_upper_case_globals)]
        static #shape_cache_ident: ::std::sync::OnceLock<
            ::std::sync::Arc<::clapfig::runtime::Shape>,
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

            fn shape_arc() -> ::std::sync::Arc<::clapfig::runtime::Shape> {
                ::clapfig::static_schema::cached_runtime_shape_arc(
                    &#shape_cache_ident,
                    || <Self as ::clapfig::Schema>::shape(),
                )
            }
        }

        #document_root_impl
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
    ident_span: proc_macro2::Span,
) -> syn::Result<Vec<TokenStream2>> {
    if data.variants.is_empty() {
        return Err(syn::Error::new(
            data.variants.span(),
            "clapfig::Schema requires at least one variant on an enum (an \
             empty enum is uninhabited and cannot be deserialized)",
        ));
    }
    // Same two-spelling rule as structs: a clapfig/serde pair must name
    // the same rule (including serde's directional deserialize form), or
    // the schema and serde's deserialize silently expect different
    // variant spellings. Either spelling alone is accepted.
    let serde_rename_all = find_serde_string_meta(type_attrs, "rename_all");
    let rename_all = resolve_rename_all_rule(
        struct_attrs.rename_all.as_ref(),
        serde_rename_all,
        ident_span,
        "variant names",
    )?;
    if let Some(rule) = &rename_all
        && !is_supported_rename_all_rule(rule)
    {
        return Err(syn::Error::new(
            ident_span,
            format!(
                "unsupported rename_all rule {rule:?}; supported: \
                 lowercase, UPPERCASE, PascalCase, camelCase, snake_case, \
                 SCREAMING_SNAKE_CASE, kebab-case, SCREAMING-KEBAB-CASE"
            ),
        ));
    }
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
        check_serde_attrs(&variant.attrs, SerdeCtx::Variant)?;
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

struct TaggedEnumExpansion {
    extra_statics: Vec<TokenStream2>,
    variants_tokens: TokenStream2,
    claim_asserts: Vec<TokenStream2>,
}

/// Internally tagged `#[serde(tag = "...")]` enum: each unit variant is
/// the empty object; each named-field struct variant is that object.
/// Discriminators follow the same rename rules as unit-only enums.
fn expand_tagged_enum(
    type_name: &syn::Ident,
    type_attrs: &[Attribute],
    struct_attrs: &StructAttrs,
    data: &DataEnum,
    tag: &str,
    ident_span: proc_macro2::Span,
) -> syn::Result<TaggedEnumExpansion> {
    if data.variants.is_empty() {
        return Err(syn::Error::new(
            data.variants.span(),
            "clapfig::Schema requires at least one variant on an enum (an \
             empty enum is uninhabited and cannot be deserialized)",
        ));
    }
    let serde_rename_all = find_serde_string_meta(type_attrs, "rename_all");
    let rename_all = resolve_rename_all_rule(
        struct_attrs.rename_all.as_ref(),
        serde_rename_all,
        ident_span,
        "variant names",
    )?;
    if let Some(rule) = &rename_all
        && !is_supported_rename_all_rule(rule)
    {
        return Err(syn::Error::new(
            ident_span,
            format!(
                "unsupported rename_all rule {rule:?}; supported: \
                 lowercase, UPPERCASE, PascalCase, camelCase, snake_case, \
                 SCREAMING_SNAKE_CASE, kebab-case, SCREAMING-KEBAB-CASE"
            ),
        ));
    }

    let mut extra_statics = Vec::new();
    let mut variant_entries = Vec::new();
    let mut claim_asserts = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for variant in &data.variants {
        check_serde_attrs(&variant.attrs, SerdeCtx::Variant)?;
        let discriminator = variant_schema_name(variant, rename_all.as_deref())?;
        // Discriminators are closed enum *values*, not dotted-path
        // segments: serde-valid spellings like `rust.v2` or `[legacy]`
        // are legal. The tag *field name* still uses
        // `validate_schema_field_name` at the `#[serde(tag)]` site.
        validate_tagged_discriminator(&discriminator, variant.ident.span())?;
        if !seen.insert(discriminator.clone()) {
            return Err(syn::Error::new(
                variant.ident.span(),
                format!(
                    "duplicate tagged discriminator {discriminator:?} after rename — \
                     two variants would produce the same schema value"
                ),
            ));
        }

        let (fields_tokens, variant_claims) = match &variant.fields {
            Fields::Unit => (quote! { &[] }, Vec::new()),
            Fields::Named(named) => {
                let mut field_entries = Vec::with_capacity(named.named.len());
                let mut field_names = std::collections::HashSet::new();
                let mut claims = Vec::new();
                for f in &named.named {
                    let expanded = expand_field(f, None)?;
                    if expanded.name == tag {
                        return Err(syn::Error::new(
                            f.ident.span(),
                            format!(
                                "tagged variant field {:?} clashes with #[serde(tag = {tag:?})] — \
                                 the tag is reserved on the object and is not a field of the \
                                 variant. Rename the field, or pick a different tag.",
                                expanded.name
                            ),
                        ));
                    }
                    if !field_names.insert(expanded.name.clone()) {
                        return Err(syn::Error::new(
                            f.ident.span(),
                            format!(
                                "duplicate schema field name {:?} — two fields (after \
                                 `rename`/`rename_all`) would collide in the schema, \
                                 making lookups and unknown-key validation \
                                 order-dependent. Rename one of them.",
                                expanded.name
                            ),
                        ));
                    }
                    claims.extend(expanded.claim_asserts);
                    field_entries.push(expanded.entry);
                }
                (quote! { &[ #(#field_entries),* ] }, claims)
            }
            Fields::Unnamed(_) => {
                return Err(syn::Error::new(
                    variant.fields.span(),
                    "internally tagged clapfig::Schema enums only support unit variants \
                     (empty object) and named-field struct variants. Tuple and newtype \
                     variants have no internally tagged object shape — use \
                     `#[clapfig(value)]` on the field that holds the union instead.",
                ));
            }
        };
        claim_asserts.extend(variant_claims);

        let variant_static_ident =
            quote::format_ident!("__CLAPFIG_SCHEMA_{}_{}", type_name, variant.ident);
        let variant_name = variant.ident.unraw().to_string();
        let variant_doc = collect_doc_lines(&variant.attrs);
        let variant_doc_expr = doc_slice(&variant_doc);
        extra_statics.push(quote! {
            #[allow(non_upper_case_globals)]
            static #variant_static_ident: ::clapfig::static_schema::SchemaStatic =
                ::clapfig::static_schema::SchemaStatic {
                    name: #variant_name,
                    doc: #variant_doc_expr,
                    strict: None,
                    fields: #fields_tokens,
                    enum_variants: &[],
                    tagged_tag: "",
                    tagged_variants: &[],
                };
        });
        variant_entries.push(quote! {
            ::clapfig::static_schema::TaggedVariantStatic {
                discriminator: #discriminator,
                schema: &#variant_static_ident,
            }
        });
    }
    Ok(TaggedEnumExpansion {
        extra_statics,
        variants_tokens: quote! { &[ #(#variant_entries),* ] },
        claim_asserts,
    })
}

/// Resolve a single variant's schema-facing name: per-variant `rename`
/// wins, otherwise the enum-level `rename_all` applies, otherwise the
/// variant ident verbatim.
fn variant_schema_name(variant: &Variant, rename_all: Option<&str>) -> syn::Result<String> {
    if let Some(name) = parse_variant_rename(variant)? {
        return Ok(name);
    }
    // `unraw()`: serde matches `r#type` against the string "type", so the
    // schema must carry the unraw spelling or every value would mismatch.
    let raw = variant.ident.unraw().to_string();
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

/// The serde `rename_all` rule set, shared by the struct-field and
/// enum-variant paths. (The two paths convert differently — field names
/// are snake_case source, variant names PascalCase source — but the rule
/// vocabulary is one set.)
fn is_supported_rename_all_rule(rule: &str) -> bool {
    matches!(
        rule,
        "lowercase"
            | "UPPERCASE"
            | "PascalCase"
            | "camelCase"
            | "snake_case"
            | "SCREAMING_SNAKE_CASE"
            | "kebab-case"
            | "SCREAMING-KEBAB-CASE"
    )
}

/// Apply a serde-compatible `rename_all` rule to a snake_case *field*
/// name, mirroring `serde_derive`'s `RenameRule::apply_to_field` exactly.
/// Distinct from [`apply_rename_all`], which converts PascalCase *variant*
/// names — the two directions share a rule vocabulary but not an
/// algorithm (e.g. `lowercase` is the identity here because a snake_case
/// source is already lowercase, while on variants it folds case).
///
/// Returns `None` for unsupported rules (callers reject those up front
/// via [`is_supported_rename_all_rule`]).
///
/// Serde-exact details worth locking down:
/// - `lowercase` and `snake_case` are the identity — serde does not touch
///   the underscores for `lowercase`.
/// - `UPPERCASE` / `SCREAMING_SNAKE_CASE` keep underscores
///   (`listen_port` → `LISTEN_PORT`).
/// - `PascalCase` capitalizes after each `_` and drops the `_`; digits
///   pass through and *consume* the capitalization (`render_2d` →
///   `Render2d`, exactly as serde does).
/// - `camelCase` is PascalCase with the first character lowercased.
fn apply_rename_all_to_field(name: &str, rule: &str) -> Option<String> {
    match rule {
        "lowercase" | "snake_case" => Some(name.to_string()),
        "UPPERCASE" | "SCREAMING_SNAKE_CASE" => Some(name.to_ascii_uppercase()),
        "PascalCase" => Some(snake_to_pascal(name)),
        "camelCase" => {
            let pascal = snake_to_pascal(name);
            let mut chars = pascal.chars();
            Some(match chars.next() {
                Some(first) => first.to_ascii_lowercase().to_string() + chars.as_str(),
                None => pascal,
            })
        }
        "kebab-case" => Some(name.replace('_', "-")),
        "SCREAMING-KEBAB-CASE" => Some(name.to_ascii_uppercase().replace('_', "-")),
        _ => None,
    }
}

/// Convert a snake_case field name to PascalCase the way serde does:
/// every `_` is dropped and capitalizes the next character (uppercasing a
/// digit is a no-op that still consumes the capitalization, so
/// `render_2d` → `Render2d`).
fn snake_to_pascal(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut capitalize = true;
    for ch in name.chars() {
        if ch == '_' {
            capitalize = true;
        } else if capitalize {
            out.push(ch.to_ascii_uppercase());
            capitalize = false;
        } else {
            out.push(ch);
        }
    }
    out
}

/// Parse `#[clapfig(rename = "name")]` or `#[serde(rename = "name")]`
/// (including serde's directional `rename(deserialize = "name", ...)`) off
/// a variant. Returns the override string or `None` if neither is present.
///
/// Any other `#[clapfig(...)]` meta on a variant is a derive-time error —
/// `rename` is the only supported variant attribute, and a typo'd meta
/// silently skipped here would let the misspelled intent (e.g.
/// `renmae = "x"`) ship as the un-renamed variant. Fields and types
/// already hard-error on unknown metas; variants are equally strict.
///
/// If both a clapfig and a serde rename are present they must agree —
/// a differing pair is a derive-time error (same contract as field-level
/// renames): the schema would carry one spelling and serde's deserialize
/// expect the other, so every value using either spelling fails somewhere.
/// The serde side goes through [`find_serde_string_meta`], so the
/// directional form contributes its `deserialize` spelling and a
/// serialize-only rename is ignored.
fn parse_variant_rename(variant: &Variant) -> syn::Result<Option<String>> {
    let mut clapfig_rename: Option<String> = None;
    for attr in &variant.attrs {
        if !attr.path().is_ident("clapfig") {
            continue;
        }
        attr.parse_nested_meta(|meta| {
            if meta.path.is_ident("rename") {
                let value: syn::LitStr = meta.value()?.parse()?;
                clapfig_rename = Some(value.value());
                Ok(())
            } else {
                Err(meta.error(format!(
                    "unsupported #[clapfig(...)] variant attribute: `{}`. \
                     Supported: rename = \"...\"",
                    meta.path
                        .get_ident()
                        .map(|i| i.to_string())
                        .unwrap_or_else(|| "?".into())
                )))
            }
        })?;
    }
    let serde_rename = find_serde_string_meta(&variant.attrs, "rename");
    match (&clapfig_rename, &serde_rename) {
        (Some(c), Some(s)) if c != s => Err(syn::Error::new(
            variant.ident.span(),
            format!(
                "#[clapfig(rename = {c:?})] conflicts with #[serde(rename = {s:?})] on \
                 this variant — the schema would accept one spelling and serde's \
                 deserialize the other. Use the same name in both, or drop the clapfig \
                 one (the schema follows serde's rename when only serde has one)."
            ),
        )),
        _ => Ok(clapfig_rename.or(serde_rename)),
    }
}

/// Resolve the active `rename_all` rule from the clapfig and serde
/// spellings. A pair naming different rules is a derive-time error —
/// otherwise the schema and serde's deserialize would disagree on
/// `converted` ("field names" or "variant names"). Either spelling
/// alone is accepted (`clapfig` converts the schema only; `serde`
/// converts both). Serde's directional deserialize form is already
/// collapsed into `serde` by [`find_serde_string_meta`].
fn resolve_rename_all_rule(
    clapfig: Option<&String>,
    serde: Option<String>,
    span: proc_macro2::Span,
    converted: &str,
) -> syn::Result<Option<String>> {
    match (clapfig, &serde) {
        (Some(c), Some(s)) if c != s => Err(syn::Error::new(
            span,
            format!(
                "#[clapfig(rename_all = {c:?})] conflicts with \
                 #[serde(rename_all = {s:?})] — the schema would convert \
                 {converted} one way and serde's deserialize the other. \
                 Use the same rule in both, or drop the clapfig one (the \
                 schema follows serde's rule when only serde has one)."
            ),
        )),
        _ => Ok(clapfig.cloned().or(serde)),
    }
}

/// Find the deserialize-facing spelling of a `#[serde(<key> = "...")]` or
/// directional `#[serde(<key>(deserialize = "...", ...))]` meta across
/// `attrs`.
///
/// Used for the enum- and struct-level `rename_all` fallbacks and the
/// field/variant-level `rename` fallbacks. Each serde attribute is parsed
/// as a comma-separated
/// `syn::Meta` list (which covers serde's attribute grammar), so a
/// preceding unrelated item (`#[serde(default, rename = "x")]`) cannot
/// hide the one we're after. Attributes that don't parse as a meta list
/// are skipped — serde's own derive is the authority on rejecting
/// malformed input.
///
/// The split `rename(serialize = ..., deserialize = ...)` form is a
/// `Meta::List`; it contributes its `deserialize` value — the schema
/// tracks what serde's deserialize expects, and the serialize spelling is
/// irrelevant to config loading. A serialize-only directional meta leaves
/// the deserialize side on the Rust spelling, so it is treated as absent.
fn serde_has_meta(attrs: &[Attribute], key: &str) -> bool {
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        let Ok(metas) = attr.parse_args_with(
            syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
        ) else {
            continue;
        };
        if metas.iter().any(|meta| meta.path().is_ident(key)) {
            return true;
        }
    }
    false
}

fn find_serde_string_meta(attrs: &[Attribute], key: &str) -> Option<String> {
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        let Ok(metas) = attr.parse_args_with(
            syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
        ) else {
            continue;
        };
        for meta in metas {
            if !meta.path().is_ident(key) {
                continue;
            }
            match meta {
                syn::Meta::NameValue(nv) => {
                    if let syn::Expr::Lit(lit) = nv.value
                        && let syn::Lit::Str(s) = lit.lit
                    {
                        return Some(s.value());
                    }
                }
                syn::Meta::List(list) => {
                    let Ok(inner) = list.parse_args_with(
                        syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
                    ) else {
                        continue;
                    };
                    for nested in inner {
                        if let syn::Meta::NameValue(nv) = nested
                            && nv.path.is_ident("deserialize")
                            && let syn::Expr::Lit(lit) = nv.value
                            && let syn::Lit::Str(s) = lit.lit
                        {
                            return Some(s.value());
                        }
                    }
                }
                syn::Meta::Path(_) => {}
            }
        }
    }
    None
}

/// Position a `#[serde(...)]` attribute appears in, for the reject-loudly
/// scan ([`check_serde_attrs`]).
#[derive(Clone, Copy)]
enum SerdeCtx {
    Struct,
    UnitEnum,
    TaggedEnum,
    Field,
    Variant,
}

impl SerdeCtx {
    fn noun(self) -> &'static str {
        match self {
            SerdeCtx::Struct => "struct",
            SerdeCtx::UnitEnum | SerdeCtx::TaggedEnum => "enum",
            SerdeCtx::Field => "field",
            SerdeCtx::Variant => "variant",
        }
    }
}

/// Serde attributes the schema either honors or that cannot make the
/// schema disagree with serde's *deserialize* (serialize-only attributes
/// and derive plumbing). Everything else is rejected by
/// [`check_serde_attrs`].
fn serde_key_allowed(key: &str, ctx: SerdeCtx) -> bool {
    match ctx {
        SerdeCtx::Field => matches!(
            key,
            // Honored: the schema follows serde's deserialize spelling.
            "rename"
            // Honored by construction: the typed deserialize runs through
            // serde on the merged table, so custom deserializers apply to
            // every source (files, env, CLI, defaults). Documented in the
            // crate docs as the normalization escape hatch. The contract
            // is shape-preserving normalization: schema validation checks
            // the field's inferred shape *before* serde runs, so a
            // shape-changing deserializer's alternate wire shape is
            // rejected loudly at validation (never silently mis-typed);
            // `#[clapfig(value)]` is the opt-out that hands the wire
            // shape to the deserializer.
            | "deserialize_with" | "with"
            // Serialize-only: the schema tracks what deserialize accepts.
            | "serialize_with" | "skip_serializing" | "skip_serializing_if"
            // Derive plumbing with no value-shape impact.
            | "bound" | "borrow"
        ),
        SerdeCtx::Variant => matches!(
            key,
            "rename" | "serialize_with" | "skip_serializing" | "bound" | "borrow"
        ),
        SerdeCtx::Struct => matches!(
            key,
            // Container rename affects serde's *type name*, which config
            // deserialization never consults.
            "rename"
            // Honored: field names are rewritten through the rule (the
            // directional form contributes its deserialize rule; a
            // serialize-only one is irrelevant to the schema). Resolved
            // against the clapfig spelling in `expand_schema`.
            | "rename_all"
            // Serialize-only / derive plumbing.
            | "into" | "bound" | "crate" | "expecting"
        ),
        SerdeCtx::UnitEnum => matches!(
            key,
            // `rename_all` is honored on unit-only enums (variant names
            // are rewritten in both the schema and serde).
            "rename" | "rename_all" | "into" | "bound" | "crate" | "expecting"
        ),
        SerdeCtx::TaggedEnum => matches!(
            key,
            // Internally tagged: `tag` is the contract (same policy as
            // `rename` / `rename_all`). `content` / `untagged` stay
            // rejected by the caller before this allowlist is used.
            "tag" | "rename" | "rename_all" | "into" | "bound" | "crate" | "expecting"
        ),
    }
}

/// Why a given unhonored serde attribute is rejected — one sentence naming
/// the divergence it would cause between the derived schema and serde's
/// deserialize, plus the supported alternative where one exists.
fn serde_key_rejection(key: &str) -> &'static str {
    match key {
        "default" => {
            "the schema does not read serde defaults, so it would still report the \
             field as missing/required on configs serde itself accepts. Use \
             `#[clapfig(default = ...)]` (injected before the typed deserialize) or an \
             `Option<T>` field"
        }
        "flatten" => {
            "the schema keeps this as a nested key while serde inlines the inner \
             fields at this level, so schema validation and the typed deserialize \
             disagree about the entire subtree"
        }
        "alias" => {
            "the schema knows exactly one spelling per field; a config written with \
             the alias spelling would pass serde but be rejected by schema validation \
             as an unknown key"
        }
        "skip" | "skip_deserializing" => {
            "the schema would still declare (and possibly require) a field that \
             serde never populates — templates and required-field checks would \
             demand a key the deserialize then ignores"
        }
        "deny_unknown_fields" => {
            "unknown-key policy is owned by clapfig's strictness cascade; serde \
             would reject keys a non-strict schema accepts. Use \
             `#[clapfig(strict = true)]` instead"
        }
        "tag" => {
            "internally tagged unions are only honored on enums via \
             `#[serde(tag = \"...\")]`; on this item it would change the value \
             shape serde deserializes — use `#[clapfig(value)]` on the field \
             that holds the union type instead"
        }
        "content" => {
            "adjacent tagging (`tag` + `content`) is not supported — only \
             internally tagged enums (`#[serde(tag = \"...\")]` without \
             `content`) are. Use `#[clapfig(value)]` on the field that holds \
             the union type instead"
        }
        "untagged" => {
            "untagged unions have no schema representation; internally tagged \
             `#[serde(tag = \"...\")]` is the supported form. Use \
             `#[clapfig(value)]` on the field that holds the union type instead"
        }
        "transparent" => {
            "the schema keeps the struct's nested shape while serde deserializes \
             the single inner field's shape directly"
        }
        "from" | "try_from" => {
            "serde deserializes via a different type whose shape the schema knows \
             nothing about"
        }
        "remote" => {
            "the derive reads this type's own fields, which is meaningless for a remote-type definition"
        }
        "other" => {
            "the schema's variant set is closed; a catch-all variant would accept \
             values the schema rejects"
        }
        "variant_identifier" | "field_identifier" => {
            "identifier enums deserialize from a different position than a config \
             value; the schema has no representation for them"
        }
        _ => {
            "the derived schema does not honor it, so the schema and serde's \
             deserialize would silently disagree"
        }
    }
}

/// Reject-loudly serde policy (epic DER01 decision): every serde attribute
/// the derived schema does not honor is a derive-time error naming the
/// attribute and the divergence it would cause. Silently ignoring them is
/// how `serde(default)` yields spurious missing-field errors and
/// `serde(flatten)` a schema that disagrees with the deserialize wholesale.
///
/// Attributes that don't parse as a comma-separated meta list are skipped —
/// serde's own derive is the authority on rejecting malformed input.
fn check_serde_attrs(attrs: &[Attribute], ctx: SerdeCtx) -> syn::Result<()> {
    for attr in attrs {
        if !attr.path().is_ident("serde") {
            continue;
        }
        let Ok(metas) = attr.parse_args_with(
            syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated,
        ) else {
            continue;
        };
        for meta in metas {
            let Some(ident) = meta.path().get_ident() else {
                continue;
            };
            let key = ident.to_string();
            if serde_key_allowed(&key, ctx) {
                continue;
            }
            return Err(syn::Error::new(
                meta.path().span(),
                format!(
                    "#[serde({key})] on a {} deriving clapfig::Schema is not supported: {}. \
                     Supporting it is out of scope by design — any serde attribute the \
                     schema does not honor is rejected at derive time rather than left to \
                     silently diverge.",
                    ctx.noun(),
                    serde_key_rejection(&key),
                ),
            ));
        }
    }
    Ok(())
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
    /// `#[clapfig(rename_all = "...")]` — rewrites every field name of a
    /// struct (fields without an explicit rename) or every variant of a
    /// unit-only enum, with serde-exact conversion semantics. Schema-side
    /// only: pairing it with the same serde rule is what makes serde's
    /// deserialize agree on the converted spellings. On structs a serde
    /// spelling that names a different rule is a derive-time error; on
    /// enums the clapfig spelling wins.
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
    min: Option<Expr>,
    max: Option<Expr>,
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
            } else if meta.path.is_ident("min") {
                let value: Expr = meta.value()?.parse()?;
                out.min = Some(value);
                Ok(())
            } else if meta.path.is_ident("max") {
                let value: Expr = meta.value()?.parse()?;
                out.max = Some(value);
                Ok(())
            } else {
                Err(meta.error(format!(
                    "unsupported #[clapfig(...)] field attribute: `{}`. \
                     Supported: default, env, rename, value, optional, allowed, min, max",
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
    Scalar(ScalarKind, TokenStream2, Option<IntegerBounds>),
    /// `Option<T>` where T is itself a TypeShape; the inner shape is folded
    /// and `optional` is set.
    Optional(Box<TypeShape>),
    /// `Vec<T>` where T is a scalar — runtime `Shape::Array` of that leaf
    /// (emits `LeafTypeStatic::Array`). Carries the element's
    /// [`ScalarKind`] so array-literal defaults can be kind-checked per
    /// element at derive time.
    Array(ScalarKind, TokenStream2),
    /// `HashMap<String, V>` / `BTreeMap<String, V>` where V is a leaf shape —
    /// runtime `Shape::Map` of that leaf (emits `LeafTypeStatic::Map`).
    /// TOML map keys must be strings.
    Map(TokenStream2),
    /// `HashMap<String, NestedStruct>` / `BTreeMap<String, NestedStruct>` —
    /// emits `FieldStatic::MapOf { schema: <NestedStruct as Schema>::STATIC, .. }`. The
    /// inner token is the same `<T as Schema>::STATIC` reference produced
    /// for plain nested fields; the converter routes it into a `Shape::Map`
    /// at the runtime layer — or, when the referenced type is a unit-only
    /// enum, flattens it to a `Shape::Map` of an enum leaf (the map-shaped
    /// sibling of the `Nested` enum flatten).
    MapOfNested(TokenStream2),
    /// `Vec<NestedType>` where the element derives [`Schema`] — emits
    /// `FieldStatic::ArrayOf { schema: <T as Schema>::STATIC, .. }`.
    /// Support is trait-resolved, not syntactic: the emitted
    /// `<T as Schema>::STATIC` reference makes the *compiler* decide
    /// whether the element type qualifies (a non-`Schema` element fails
    /// with the trait's `on_unimplemented` guidance). The converter routes
    /// it into a `Shape::Array` at the runtime layer — or, when the
    /// element type is a unit-only enum, flattens it to a
    /// `Shape::Array` of an enum leaf (the array-shaped sibling of the
    /// `MapOf` enum flatten).
    ArrayOfNested(TokenStream2),
    /// Nested type referencing another struct's `clapfig::Schema` impl.
    Nested(TokenStream2),
    /// `clapfig::value::Value` — emits `LeafType::Value`.
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct IntegerBounds {
    min: Option<i64>,
    max: Option<i64>,
    target: IntegerTarget,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum IntegerTarget {
    Fixed,
    Isize,
    Usize,
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
        // Vec<T>: a scalar T is a leaf array; any other type path is
        // treated as a nested schema type and emitted as an `ArrayOf`
        // reference — the *compiler* resolves whether T actually derives
        // `Schema` (trait-resolved support, not a syntactic guess).
        let inner_shape = classify_type(inner)?;
        return match inner_shape {
            TypeShape::Scalar(kind, tok, _) => Ok(TypeShape::Array(kind, tok)),
            TypeShape::Nested(expr) => Ok(TypeShape::ArrayOfNested(expr)),
            TypeShape::Optional(_) => Err(syn::Error::new(
                inner.span(),
                "Vec<Option<T>> is not supported — an absent element has no TOML \
                 representation; use Option<Vec<T>> for an optional list, or \
                 `#[clapfig(value)]` with `clapfig::value::Value` for free-form shapes",
            )),
            TypeShape::Array(_, _) | TypeShape::ArrayOfNested(_) => Err(syn::Error::new(
                inner.span(),
                "nested arrays (Vec<Vec<...>>) are not supported by clapfig::Schema. \
                 Use `#[clapfig(value)]` with `clapfig::value::Value` for free-form \
                 nested shapes.",
            )),
            TypeShape::Value => Err(syn::Error::new(
                inner.span(),
                "Vec<clapfig::value::Value> is not supported; use a single \
                 `clapfig::value::Value` with #[clapfig(value)] instead",
            )),
            TypeShape::Map(_) | TypeShape::MapOfNested(_) => Err(syn::Error::new(
                inner.span(),
                "Vec<HashMap<...>> / Vec<BTreeMap<...>> is not supported by clapfig::Schema. \
                 Use `#[clapfig(value)]` with `clapfig::value::Value` for free-form nested shapes.",
            )),
        };
    }
    if name == "HashMap" || name == "BTreeMap" {
        let (key_ty, value_ty) = two_generic_arguments(&last.arguments, &name)?;
        // TOML map keys are strings — `Shape::Map` of a leaf has no key-type
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
            TypeShape::Scalar(_, tok, _) => Ok(TypeShape::Map(tok)),
            TypeShape::Value => Ok(TypeShape::Map(
                quote! { ::clapfig::static_schema::LeafTypeStatic::Value },
            )),
            TypeShape::Array(_, elem) => Ok(TypeShape::Map(
                quote! { ::clapfig::static_schema::LeafTypeStatic::Array(&#elem) },
            )),
            TypeShape::ArrayOfNested(_) => Err(syn::Error::new(
                value_ty.span(),
                format!(
                    "{name}<String, Vec<NestedType>> (map of arrays of nested schema types) \
                     is not supported by clapfig::Schema. Use `#[clapfig(value)]` with \
                     `clapfig::value::Value` for free-form nested shapes."
                ),
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
                     Use `#[clapfig(value)]` with `clapfig::value::Value` for free-form nested shapes."
                ),
            )),
            // {Hash,BTree}Map<String, NestedStruct> → `FieldStatic::MapOf`.
            // The inner expression is the same `<T as Schema>::STATIC`
            // we use for plain Nested fields; the converter sees a `MapOf` and
            // emits `Shape::Map` of that object.
            TypeShape::Nested(inner_expr) => Ok(TypeShape::MapOfNested(inner_expr)),
        };
    }

    if name == "Value" && is_clapfig_value_path(path) {
        return Ok(TypeShape::Value);
    }
    if name == "Datetime" && is_clapfig_datetime_path(path) {
        return Ok(TypeShape::Scalar(
            ScalarKind::DateTime,
            quote! { ::clapfig::static_schema::LeafTypeStatic::DateTime },
            None,
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
                 `clapfig::value::Value` for a free-form leaf."
            ),
        ));
    }
    let scalar = match name.as_str() {
        "String" => Some((
            ScalarKind::String,
            quote! { ::clapfig::static_schema::LeafTypeStatic::String },
            None,
        )),
        "bool" => Some((
            ScalarKind::Bool,
            quote! { ::clapfig::static_schema::LeafTypeStatic::Bool },
            None,
        )),
        // Every Rust integer maps to TOML's single Integer width, carrying
        // the source width's bounds so out-of-range values fail the schema
        // check with the key path (and export as JSON Schema
        // `minimum`/`maximum`). Pointer-width integers emit target-side
        // expressions so cross-compilation uses the target's `isize` /
        // `usize` limits.
        "i8" | "i16" | "i32" | "i64" | "u8" | "u16" | "u32" | "u64" | "usize" | "isize" => {
            let bounds = integer_bounds(&name);
            let (min, max) = integer_bounds_tokens(bounds);
            Some((
                ScalarKind::Integer,
                quote! { ::clapfig::static_schema::LeafTypeStatic::Integer { min: #min, max: #max } },
                Some(bounds),
            ))
        }
        "f32" | "f64" => Some((
            ScalarKind::Float,
            quote! { ::clapfig::static_schema::LeafTypeStatic::Float },
            None,
        )),
        _ => None,
    };
    if let Some((kind, tok, bounds)) = scalar {
        return Ok(TypeShape::Scalar(kind, tok, bounds));
    }

    // Default: treat as a nested struct that also implements clapfig::Schema.
    // Use the associated const STATIC (not schema_static()) so the parent's
    // `static SchemaStatic = ...` initializer can compose it in const
    // context — trait fns are not callable from const on stable Rust.
    let nested = quote! { <#ty as ::clapfig::Schema>::STATIC };
    Ok(TypeShape::Nested(nested))
}

/// Macro-time bounds for a Rust integer type name.
///
/// Fixed-width types use their exact ranges. Pointer-width integers use
/// their widest possible target range here, then `integer_bounds_tokens`
/// emits target-side expressions that intersect with the actual target.
fn integer_bounds(name: &str) -> IntegerBounds {
    match name {
        "i8" => IntegerBounds {
            min: Some(i8::MIN as i64),
            max: Some(i8::MAX as i64),
            target: IntegerTarget::Fixed,
        },
        "i16" => IntegerBounds {
            min: Some(i16::MIN as i64),
            max: Some(i16::MAX as i64),
            target: IntegerTarget::Fixed,
        },
        "i32" => IntegerBounds {
            min: Some(i32::MIN as i64),
            max: Some(i32::MAX as i64),
            target: IntegerTarget::Fixed,
        },
        "i64" => IntegerBounds {
            min: None,
            max: None,
            target: IntegerTarget::Fixed,
        },
        "isize" => IntegerBounds {
            min: None,
            max: None,
            target: IntegerTarget::Isize,
        },
        "u8" => IntegerBounds {
            min: Some(0),
            max: Some(u8::MAX as i64),
            target: IntegerTarget::Fixed,
        },
        "u16" => IntegerBounds {
            min: Some(0),
            max: Some(u16::MAX as i64),
            target: IntegerTarget::Fixed,
        },
        "u32" => IntegerBounds {
            min: Some(0),
            max: Some(u32::MAX as i64),
            target: IntegerTarget::Fixed,
        },
        "u64" => IntegerBounds {
            min: Some(0),
            max: None,
            target: IntegerTarget::Fixed,
        },
        "usize" => IntegerBounds {
            min: Some(0),
            max: None,
            target: IntegerTarget::Usize,
        },
        other => unreachable!("integer_bounds called for non-integer {other}"),
    }
}

fn integer_bounds_tokens(bounds: IntegerBounds) -> (TokenStream2, TokenStream2) {
    match bounds.target {
        IntegerTarget::Fixed => (option_i64_tokens(bounds.min), option_i64_tokens(bounds.max)),
        IntegerTarget::Isize => {
            let target_min = quote! {
                if isize::BITS < 64 { Some(isize::MIN as i64) } else { None }
            };
            let target_max = quote! {
                if isize::BITS < 64 { Some(isize::MAX as i64) } else { None }
            };
            pointer_bounds_tokens(target_min, target_max, bounds.min, bounds.max)
        }
        IntegerTarget::Usize => {
            let target_min = quote! { Some(0) };
            let target_max = quote! {
                if usize::BITS < 64 { Some(usize::MAX as i64) } else { None }
            };
            pointer_bounds_tokens(target_min, target_max, bounds.min, bounds.max)
        }
    }
}

fn option_i64_tokens(value: Option<i64>) -> TokenStream2 {
    match value {
        Some(value) => quote! { Some(#value) },
        None => quote! { None },
    }
}

fn pointer_bounds_tokens(
    target_min: TokenStream2,
    target_max: TokenStream2,
    declared_min: Option<i64>,
    declared_max: Option<i64>,
) -> (TokenStream2, TokenStream2) {
    let min = intersect_min_tokens(target_min.clone(), declared_min);
    let max = intersect_max_tokens(target_max.clone(), declared_max);
    let asserted_min = quote! {{
        let min = #min;
        let max = #max;
        if let (Some(min), Some(max)) = (min, max) {
            assert!(
                min <= max,
                "`min`/`max` contradict the Rust integer type's representable range on this target"
            );
        }
        min
    }};
    (asserted_min, max)
}

fn pointer_default_assertion_tokens(
    shape: &TypeShape,
    attrs: &FieldAttrs,
    default: &Expr,
    span: proc_macro2::Span,
) -> syn::Result<TokenStream2> {
    let Some(LitValue::Int(default_value)) = parse_lit_value(default) else {
        return Ok(quote! {});
    };
    let Ok((inferred, _)) = integer_bounds_for_shape(shape, span) else {
        return Ok(quote! {});
    };
    let bounds = declared_integer_bounds(attrs, inferred, span)?;
    if bounds.target == IntegerTarget::Fixed {
        return Ok(quote! {});
    }

    let (min, max) = integer_bounds_tokens(bounds);
    Ok(quote! {
        let min = #min;
        let max = #max;
        if let Some(min) = min {
            assert!(
                #default_value >= min,
                "`default = ...` is outside the field's integer range on this target"
            );
        }
        if let Some(max) = max {
            assert!(
                #default_value <= max,
                "`default = ...` is outside the field's integer range on this target"
            );
        }
    })
}

fn intersect_min_tokens(target_min: TokenStream2, declared_min: Option<i64>) -> TokenStream2 {
    match declared_min {
        Some(declared) => quote! {
            match #target_min {
                Some(target) if target > #declared => Some(target),
                _ => Some(#declared),
            }
        },
        None => target_min,
    }
}

fn intersect_max_tokens(target_max: TokenStream2, declared_max: Option<i64>) -> TokenStream2 {
    match declared_max {
        Some(declared) => quote! {
            match #target_max {
                Some(target) if target < #declared => Some(target),
                _ => Some(#declared),
            }
        },
        None => target_max,
    }
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

fn is_clapfig_value_path(path: &syn::Path) -> bool {
    // Strict suffix match for clapfig's owned `Value` type
    // (`clapfig::value::Value`, the value-model lingua franca):
    //   `Value`                        — assumed use-imported
    //   `value::Value`                 — module-qualified
    //   `clapfig::value::Value`        — canonical form (incl. leading `::`)
    // Anything else (e.g. `my_crate::Value`, the toml crate's `Value`, or a longer
    // path ending in `value::Value`) is rejected. The leading-colon form
    // parses as the same segments — `syn::Path::leading_colon` is a
    // separate field, not a segment.
    let segs: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    matches!(segs.as_slice(),
        [a] if a == "Value"
    ) || matches!(segs.as_slice(),
        [a, b] if a == "value" && b == "Value"
    ) || matches!(segs.as_slice(),
        [a, b, c] if a == "clapfig" && b == "value" && c == "Value"
    )
}

fn is_clapfig_datetime_path(path: &syn::Path) -> bool {
    // Strict suffix match for `clapfig::value::Datetime`:
    //   `Datetime`                     — use-imported
    //   `value::Datetime`              — module-qualified
    //   `clapfig::value::Datetime`     — canonical
    // Any other path is rejected — the toml crate's `Datetime` and friends do
    // NOT match (the format types are confined to their adapters).
    let segs: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    matches!(segs.as_slice(),
        [a] if a == "Datetime"
    ) || matches!(segs.as_slice(),
        [a, b] if a == "value" && b == "Datetime"
    ) || matches!(segs.as_slice(),
        [a, b, c] if a == "clapfig" && b == "value" && c == "Datetime"
    )
}

/// Collect compile-time marker-trait assertions for every type path the
/// macro claims *by name* — `Datetime` → datetime leaf, `Value` →
/// free-form leaf. A proc macro cannot resolve names, so a user's own
/// `struct Datetime` (or `Value`) at a claimed position would otherwise
/// silently become the wrong leaf type. The emitted `const` turns the
/// mismatch into a compile error whose message comes from the
/// `IsClapfigDatetime` / `IsClapfigValue` `on_unimplemented` diagnostics,
/// spanned at the offending field type.
///
/// Recurses through generic arguments so wrapped positions
/// (`Option<Datetime>`, `Vec<Datetime>`, `HashMap<String, Value>`) are
/// covered too.
fn collect_type_claim_assertions(ty: &Type, out: &mut Vec<TokenStream2>) {
    let Type::Path(TypePath { path, qself: None }) = ty else {
        return;
    };
    let span = ty.span();
    if is_clapfig_datetime_path(path) {
        out.push(quote::quote_spanned! {span=>
            const _: fn() = || {
                fn claimed_as_clapfig_datetime<
                    T: ::clapfig::static_schema::IsClapfigDatetime,
                >() {}
                let _ = claimed_as_clapfig_datetime::<#ty>;
            };
        });
        return;
    }
    if is_clapfig_value_path(path) {
        out.push(quote::quote_spanned! {span=>
            const _: fn() = || {
                fn claimed_as_clapfig_value<
                    T: ::clapfig::static_schema::IsClapfigValue,
                >() {}
                let _ = claimed_as_clapfig_value::<#ty>;
            };
        });
        return;
    }
    if let Some(last) = path.segments.last()
        && let PathArguments::AngleBracketed(args) = &last.arguments
    {
        for arg in &args.args {
            if let GenericArgument::Type(t) = arg {
                collect_type_claim_assertions(t, out);
            }
        }
    }
}

/// One expanded struct field: the resolved schema-facing name (for the
/// caller's duplicate check), the `NamedFieldStatic` initializer entry,
/// and any module-level type-claim assertions to emit alongside the
/// schema statics.
struct ExpandedField {
    name: String,
    entry: TokenStream2,
    claim_asserts: Vec<TokenStream2>,
}

fn expand_field(field: &syn::Field, rename_all: Option<&str>) -> syn::Result<ExpandedField> {
    let ident = field
        .ident
        .as_ref()
        .ok_or_else(|| syn::Error::new(field.span(), "expected named field"))?;
    let attrs = parse_field_attrs(&field.attrs)?;
    check_serde_attrs(&field.attrs, SerdeCtx::Field)?;
    let doc_lines = collect_doc_lines(&field.attrs);
    // Schema-facing field name: `#[clapfig(rename)]` if present, else
    // `#[serde(rename)]` (the schema follows serde so the merged config and
    // the typed deserialize agree on one spelling), else the Rust identifier
    // (unraw'd: serde spells `r#type` as "type", so the schema must too)
    // rewritten through the struct-level `rename_all` rule when one is set.
    // Precedence matches serde: an explicit rename — including a
    // deserialize-side directional one — exempts the field from the rule;
    // a serialize-only directional rename is invisible here, so the rule
    // still applies to the deserialize side, exactly as serde behaves.
    // A differing clapfig/serde pair is a hard error — the schema would
    // expect one spelling and serde's deserialize the other, so every load
    // would fail at runtime.
    let serde_rename = find_serde_string_meta(&field.attrs, "rename");
    let name = match (&attrs.rename, &serde_rename) {
        (Some(c), Some(s)) if c != s => {
            return Err(syn::Error::new(
                ident.span(),
                format!(
                    "#[clapfig(rename = {c:?})] conflicts with #[serde(rename = {s:?})] — \
                     the schema would expect one spelling and serde's deserialize the \
                     other. Use the same name in both, or drop the clapfig one (the \
                     schema follows serde's rename when only serde has one)."
                ),
            ));
        }
        (Some(c), _) => {
            validate_schema_field_name(c, "#[clapfig(rename)]", ident.span())?;
            c.clone()
        }
        (None, Some(s)) => {
            validate_schema_field_name(s, "#[serde(rename)]", ident.span())?;
            s.clone()
        }
        (None, None) => match rename_all {
            Some(rule) => {
                let converted = apply_rename_all_to_field(&ident.unraw().to_string(), rule)
                    .expect("rule validated before field expansion");
                // Rule conversions of valid Rust identifiers are almost
                // always valid schema names, but not provably so for every
                // input (PascalCase of an all-underscore identifier like
                // `__` converts to the empty string) — run them through
                // the same validation as explicit renames.
                validate_schema_field_name(
                    &converted,
                    &format!("rename_all = {rule:?}"),
                    ident.span(),
                )?;
                converted
            }
            None => ident.unraw().to_string(),
        },
    };
    let doc_expr = doc_slice(&doc_lines);
    // Type-claim assertions for by-name matches (`Datetime` / `Value`).
    // Skipped under `#[clapfig(value)]`: the fast path never runs the
    // by-name inference, so nothing is claimed.
    let mut claim_asserts = Vec::new();
    if !attrs.force_value {
        collect_type_claim_assertions(&field.ty, &mut claim_asserts);
    }

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
        if has_range_attrs(&attrs) {
            return Err(syn::Error::new(
                field.span(),
                "`min` and `max` are only valid on integer fields",
            ));
        }
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
                            None,
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
            return Ok(ExpandedField {
                name: name.clone(),
                entry: quote! {
                    ::clapfig::static_schema::NamedFieldStatic {
                        name: #name,
                        field: ::clapfig::static_schema::FieldStatic::Leaf(#leaf),
                    }
                },
                claim_asserts,
            });
        }
        // Bare nested with no leaf attrs — original path. Field-site doc
        // rides along so the converter can prefer it over the type's own
        // doc (previously it was silently dropped, asymmetric with the
        // `Option<Mode>` EnumRef path which keeps its field doc).
        return Ok(ExpandedField {
            name: name.clone(),
            entry: quote! {
                ::clapfig::static_schema::NamedFieldStatic {
                    name: #name,
                    field: ::clapfig::static_schema::FieldStatic::Nested {
                        schema: #inner_expr,
                        doc: #doc_expr,
                    },
                }
            },
            claim_asserts,
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
            || has_range_attrs(&attrs)
            || attrs.optional
        {
            return Err(syn::Error::new(
                field.span(),
                "leaf attributes (default, env, allowed, min, max, optional) are not \
                 valid on map-of-nested-struct fields — entry presence is \
                 already user-controlled, and a single per-field default \
                 has no meaning across an arbitrary set of entry keys.",
            ));
        }
        return Ok(ExpandedField {
            name: name.clone(),
            entry: quote! {
                ::clapfig::static_schema::NamedFieldStatic {
                    name: #name,
                    field: ::clapfig::static_schema::FieldStatic::MapOf {
                        schema: #inner_expr,
                        doc: #doc_expr,
                    },
                }
            },
            claim_asserts,
        });
    }

    // `Vec<T>` where `T` is a nested schema type — bare fields emit the
    // structural `FieldStatic::ArrayOf` (the converter flattens a
    // unit-only-enum element type to an `Array(Enum)` leaf);
    // `Option<Vec<T>>` fields emit an optional `Array(EnumRef)` leaf,
    // representable only for the enum kind. Leaf attrs are a hard error
    // either way — same rule as map-of-nested fields below.
    let array_inner_expr: Option<(&TokenStream2, bool)> = match &shape {
        TypeShape::ArrayOfNested(inner) => Some((inner, false)),
        TypeShape::Optional(inner) => match inner.as_ref() {
            TypeShape::ArrayOfNested(expr) => Some((expr, true)),
            _ => None,
        },
        _ => None,
    };
    if let Some((inner_expr, is_optional_wrapped)) = array_inner_expr {
        if attrs.default.is_some()
            || attrs.env.is_some()
            || attrs.allowed.is_some()
            || has_range_attrs(&attrs)
            || attrs.optional
        {
            return Err(syn::Error::new(
                field.span(),
                "leaf attributes (default, env, allowed, min, max, optional) are not \
                 valid on array-of-nested-schema fields — array entries are \
                 user-supplied (an absent array is the empty array), and a \
                 per-field scalar attribute has no meaning across a list of \
                 entries. For an optional list of a unit-only enum, use \
                 `Option<Vec<T>>`.",
            ));
        }
        if is_optional_wrapped {
            // `Option<Vec<T>>` is representable only when `T` is a
            // unit-only enum (an optional array-of-enum leaf; absent →
            // `None`). The macro can't tell enum from struct at the
            // field site, so it routes through `Array(EnumRef)`; the
            // converter checks the kind at the first `schema()` call
            // and panics with drop-the-`Option` guidance for structs
            // (an absent array of nested objects is already the empty
            // array). Same deferred-error pattern as `Option<Nested>`.
            return Ok(ExpandedField {
                name: name.clone(),
                entry: quote! {
                    ::clapfig::static_schema::NamedFieldStatic {
                        name: #name,
                        field: ::clapfig::static_schema::FieldStatic::Leaf(
                            ::clapfig::static_schema::LeafStatic {
                                doc: #doc_expr,
                                ty: ::clapfig::static_schema::LeafTypeStatic::Array(
                                    &::clapfig::static_schema::LeafTypeStatic::EnumRef {
                                        schema: #inner_expr,
                                        field_name: #name,
                                    },
                                ),
                                default: None,
                                optional: true,
                                env: None,
                            }
                        ),
                    }
                },
                claim_asserts,
            });
        }
        return Ok(ExpandedField {
            name: name.clone(),
            entry: quote! {
                ::clapfig::static_schema::NamedFieldStatic {
                    name: #name,
                    field: ::clapfig::static_schema::FieldStatic::ArrayOf {
                        schema: #inner_expr,
                        doc: #doc_expr,
                    },
                }
            },
            claim_asserts,
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
            let target_assertion =
                pointer_default_assertion_tokens(&shape, &attrs, expr, field.span())?;
            quote! {{
                #target_assertion
                Some(#v)
            }}
        }
        // Bare (non-`Option`) map-typed leaves carry no default, yet an
        // absent map still loads as the empty map: `fill_defaults`
        // materializes `{}` for non-optional map leaves, so the typed
        // deserialize produces an empty `HashMap`/`BTreeMap` instead of a
        // missing-field error — the rule the structural `MapOf` shape
        // follows, and what makes the "an absent map is already the empty
        // map" remediation in the map-default rejection true.
        // (`Option<Map<..>>` leaves are optional: absence stays absent and
        // deserializes to `None`.)
        None => quote! { None },
    };

    // A default outside the `allowed = [...]` set could never validate at
    // load — reject the contradiction at derive time. (Enum-typed fields
    // defer the same check to the converter: their variant set lives on a
    // different type the macro can't see.)
    if let (Some(allowed), Some(default)) = (&attrs.allowed, &attrs.default)
        && let Some(default_value) = parse_lit_value(default)
        && !allowed
            .iter()
            .filter_map(parse_lit_value)
            .any(|a| a == default_value)
    {
        return Err(syn::Error::new(
            default.span(),
            "`default = ...` is not one of the `allowed = [...]` values, so the \
             default could never validate at load. Add it to `allowed` or pick one \
             of the listed values.",
        ));
    }
    if let Some(default) = &attrs.default
        && let Some(LitValue::Int(value)) = parse_lit_value(default)
        && let Ok((bounds, _)) = integer_bounds_for_shape(&shape, field.span())
    {
        let bounds = declared_integer_bounds(&attrs, bounds, field.span())?;
        if bounds.min.is_some_and(|min| value < min) || bounds.max.is_some_and(|max| value > max) {
            return Err(syn::Error::new(
                default.span(),
                "`default = ...` is outside the field's integer range",
            ));
        }
    }

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

    Ok(ExpandedField {
        name: name.clone(),
        entry: quote! {
            ::clapfig::static_schema::NamedFieldStatic {
                name: #name,
                field: ::clapfig::static_schema::FieldStatic::Leaf(#leaf),
            }
        },
        claim_asserts,
    })
}

/// Derive-time mirror of the runtime's `validate_field_name`: schema field
/// names must be non-empty and free of `.` / `[` / `]`, or every
/// downstream consumer (dotted-path resolve, persist, the strictness
/// cascade) mis-parses them. The runtime builder asserts this on
/// construction, but the macro's const emission never goes through the
/// builder — so an invalid `rename` would otherwise ship silently and
/// misbehave at load.
///
/// `source` names the attribute the string came from, for the diagnostic.
/// Rust-identifier-derived names can't violate these rules and skip this
/// check.
///
/// Not used for tagged-union discriminator *values* — those are a closed
/// enum set, not path segments. See [`validate_tagged_discriminator`].
fn validate_schema_field_name(
    name: &str,
    source: &str,
    span: proc_macro2::Span,
) -> syn::Result<()> {
    let problem = if name.is_empty() {
        Some("is empty".to_string())
    } else if name.contains('.') {
        Some("contains '.', which conflicts with the dotted-path separator".to_string())
    } else {
        name.chars()
            .find(|c| *c == '[' || *c == ']')
            .map(|c| format!("contains {c:?}, which conflicts with array-index syntax"))
    };
    match problem {
        Some(problem) => Err(syn::Error::new(
            span,
            format!(
                "invalid schema field name {name:?} from {source}: the name {problem}. \
                 Pick a non-empty name without '.', '[' or ']'."
            ),
        )),
        None => Ok(()),
    }
}

/// Tagged discriminators must be non-empty; uniqueness is the caller's
/// post-rename `seen` set. Path characters (`.`, `[`, `]`) are legal —
/// the discriminator is a value, matching the runtime builder and serde.
fn validate_tagged_discriminator(name: &str, span: proc_macro2::Span) -> syn::Result<()> {
    if name.is_empty() {
        Err(syn::Error::new(
            span,
            "invalid tagged discriminator \"\": discriminators must be non-empty",
        ))
    } else {
        Ok(())
    }
}

/// Derive-time comparable form of a literal (or unary-negated literal)
/// expression. Used for the `default`-in-`allowed` membership check.
/// Returns `None` for anything that isn't a plain scalar literal — those
/// cases are already rejected (or bounds-checked) by the emission path,
/// so membership silently passes rather than double-reporting.
#[derive(PartialEq)]
enum LitValue {
    Str(String),
    Int(i64),
    Float(f64),
    Bool(bool),
}

fn parse_lit_value(expr: &Expr) -> Option<LitValue> {
    match expr {
        Expr::Lit(ExprLit { lit, .. }) => match lit {
            Lit::Str(s) => Some(LitValue::Str(s.value())),
            Lit::Int(i) => parse_signed_integer_literal(i, false)
                .ok()
                .map(LitValue::Int),
            Lit::Float(f) => f.base10_parse().ok().map(LitValue::Float),
            Lit::Bool(b) => Some(LitValue::Bool(b.value)),
            _ => None,
        },
        Expr::Unary(syn::ExprUnary {
            op: syn::UnOp::Neg(_),
            expr: inner,
            ..
        }) => match inner.as_ref() {
            Expr::Lit(ExprLit {
                lit: Lit::Int(i), ..
            }) => parse_signed_integer_literal(i, true)
                .ok()
                .map(LitValue::Int),
            _ => match parse_lit_value(inner)? {
                LitValue::Float(f) => Some(LitValue::Float(-f)),
                _ => None,
            },
        },
        _ => None,
    }
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
        if attrs.min.is_some() || attrs.max.is_some() {
            return Err(syn::Error::new(
                span,
                "`min` and `max` are only valid on integer fields, not `#[clapfig(value)]` fields",
            ));
        }
        let (_, optional_from_type) = inner_leaf_type(shape)?;
        return Ok((
            quote! { ::clapfig::static_schema::LeafTypeStatic::Value },
            optional_from_type,
        ));
    }
    if let Some(allowed) = &attrs.allowed {
        if attrs.min.is_some() || attrs.max.is_some() {
            return Err(syn::Error::new(
                span,
                "`allowed` cannot be combined with `min` or `max`; `allowed` emits an enum leaf, not a range-bounded integer leaf",
            ));
        }
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
    if attrs.min.is_some() || attrs.max.is_some() {
        let (bounds, optional_from_type) = integer_bounds_for_shape(shape, span)?;
        let bounds = declared_integer_bounds(attrs, bounds, span)?;
        let (min, max) = integer_bounds_tokens(bounds);
        return Ok((
            quote! { ::clapfig::static_schema::LeafTypeStatic::Integer { min: #min, max: #max } },
            optional_from_type,
        ));
    }
    inner_leaf_type(shape)
}

fn has_range_attrs(attrs: &FieldAttrs) -> bool {
    attrs.min.is_some() || attrs.max.is_some()
}

fn integer_bounds_for_shape(
    shape: &TypeShape,
    span: proc_macro2::Span,
) -> syn::Result<(IntegerBounds, bool)> {
    match shape {
        TypeShape::Scalar(ScalarKind::Integer, _, Some(bounds)) => Ok((*bounds, false)),
        TypeShape::Optional(inner) => {
            let (bounds, _) = integer_bounds_for_shape(inner, span)?;
            Ok((bounds, true))
        }
        _ => Err(syn::Error::new(
            span,
            "`min` and `max` are only valid on integer fields",
        )),
    }
}

fn declared_integer_bounds(
    attrs: &FieldAttrs,
    inferred: IntegerBounds,
    span: proc_macro2::Span,
) -> syn::Result<IntegerBounds> {
    let declared_min = attrs
        .min
        .as_ref()
        .map(integer_bound_from_expr)
        .transpose()?;
    let declared_max = attrs
        .max
        .as_ref()
        .map(integer_bound_from_expr)
        .transpose()?;
    if let (Some(min), Some(max)) = (declared_min, declared_max)
        && min > max
    {
        return Err(syn::Error::new(
            span,
            "`min` must be less than or equal to `max`",
        ));
    }
    let min = match (inferred.min, declared_min) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    };
    let max = match (inferred.max, declared_max) {
        (Some(a), Some(b)) => Some(a.min(b)),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    };
    if let (Some(min), Some(max)) = (min, max)
        && min > max
    {
        return Err(syn::Error::new(
            span,
            "`min`/`max` contradict the Rust integer type's representable range",
        ));
    }
    Ok(IntegerBounds {
        min,
        max,
        target: inferred.target,
    })
}

fn integer_bound_from_expr(expr: &Expr) -> syn::Result<i64> {
    match parse_lit_value(expr) {
        Some(LitValue::Int(value)) => Ok(value),
        _ => Err(syn::Error::new(
            expr.span(),
            "`min` and `max` require integer literals",
        )),
    }
}

/// `allowed = [...]` is only meaningful on scalar leaves; otherwise the
/// emitted schema would be self-contradictory (enum-of-string constraint
/// on a Vec field, etc.).
fn shape_accepts_allowed(shape: &TypeShape) -> bool {
    match shape {
        TypeShape::Scalar(_, _, _) => true,
        TypeShape::Optional(inner) => shape_accepts_allowed(inner),
        TypeShape::Array(_, _)
        | TypeShape::ArrayOfNested(_)
        | TypeShape::Map(_)
        | TypeShape::MapOfNested(_)
        | TypeShape::Value
        | TypeShape::Nested(_) => false,
    }
}

fn inner_leaf_type(shape: &TypeShape) -> syn::Result<(TokenStream2, bool)> {
    match shape {
        TypeShape::Scalar(_, tok, _) => Ok((tok.clone(), false)),
        TypeShape::Optional(inner) => {
            let (inner_tok, _) = inner_leaf_type(inner)?;
            Ok((inner_tok, true))
        }
        TypeShape::Array(_, elem) => Ok((
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
        TypeShape::Nested(_) | TypeShape::MapOfNested(_) | TypeShape::ArrayOfNested(_) => {
            unreachable!(
                "nested / map-of-nested / array-of-nested handled before leaf-type dispatch"
            )
        }
    }
}

/// Extract the scalar kind from a (possibly `Option`-wrapped) scalar shape.
/// Returns `None` for non-scalar shapes — used by `allowed`-attribute
/// validation to check that literal types match the field's TOML kind.
fn scalar_kind_of(shape: &TypeShape) -> Option<ScalarKind> {
    match shape {
        TypeShape::Scalar(k, _, _) => Some(*k),
        TypeShape::Optional(inner) => scalar_kind_of(inner),
        _ => None,
    }
}

/// Parse a literal-or-negated-literal expression into a `ValueStatic`
/// token, without kind validation. Used inside array-literal defaults
/// where the element type is already constrained by the field's `Vec<T>`
/// declaration (the per-element kind check is done by the value
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

/// Parse a literal (an `allowed = [...]` entry, a `default = ...`, or a
/// default-array element) against the field's scalar kind.
///
/// Accepts positive literals (`"x"`, `1`, `1.5`, `true`) and unary-negated
/// numeric literals (`-1`, `-1.5`). Rejects literals whose TOML primitive
/// type doesn't match the field — e.g. `allowed = ["a"]` or
/// `default = "a"` on an `i64` field — so the emitted schema is consistent
/// with what the deserializer can accept.
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
                    "expected a literal TOML primitive",
                ));
            }
        }
        _ => {
            return Err(syn::Error::new(
                expr.span(),
                "expected a literal TOML primitive (string, integer, float, bool)",
            ));
        }
    };
    if literal_kind != kind {
        return Err(syn::Error::new(
            expr.span(),
            format!(
                "literal has TOML type `{}` but the field is `{}`; \
                 the value could never pass the field's type check.",
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

/// Materialize a `ValueStatic` for a `#[clapfig(default = ...)]` expression,
/// kind-checked against the field's classified shape (same contract as
/// `allowed = [...]` literals): a default whose TOML type can't match the
/// field — `default = "hello"` on a `u16`, `default = 1` on a `Vec<String>`
/// — is a derive-time error instead of compiling and failing at every load.
fn expr_to_value_static(expr: &Expr, shape: &TypeShape) -> syn::Result<TokenStream2> {
    // Optionality doesn't change the default's kind.
    let shape = match shape {
        TypeShape::Optional(inner) => inner.as_ref(),
        s => s,
    };
    match shape {
        TypeShape::Scalar(kind, _, _) => scalar_default_to_value_static(expr, *kind),
        TypeShape::Array(elem_kind, _) => match expr {
            Expr::Array(a) => {
                let items: Vec<TokenStream2> = a
                    .elems
                    .iter()
                    .map(|e| scalar_default_to_value_static(e, *elem_kind))
                    .collect::<syn::Result<_>>()?;
                Ok(quote! {
                    ::clapfig::static_schema::ValueStatic::Array(&[ #(#items),* ])
                })
            }
            other => Err(syn::Error::new(
                other.span(),
                "defaults on Vec<T> fields must be array literals \
                 (`default = [\"a\", \"b\"]`); a scalar default would fail the \
                 array type-check at every load",
            )),
        },
        // `#[clapfig(value)]` leaves accept any TOML shape, so any literal
        // (or array of literals) is a valid default.
        TypeShape::Value => value_default_to_value_static(expr),
        TypeShape::Map(_) => Err(syn::Error::new(
            expr.span(),
            "`#[clapfig(default = ...)]` is not supported on map-typed fields — \
             there is no literal syntax for a map default, and a scalar or array \
             default would fail the map type-check at every load. Omit the \
             attribute; an absent map is already the empty map.",
        )),
        TypeShape::Optional(_)
        | TypeShape::Nested(_)
        | TypeShape::MapOfNested(_)
        | TypeShape::ArrayOfNested(_) => {
            unreachable!(
                "Optional unwrapped above; Nested/MapOf/ArrayOf handled before leaf emission"
            )
        }
    }
}

/// Kind-checked scalar default (also used per element for array-literal
/// defaults on `Vec<T>`).
///
/// Datetime fields route string literals to `ValueStatic::Datetime` so the
/// runtime's `LeafType::DateTime` check accepts the default at finalize.
/// The literal is *not* parsed at derive time — the macro intentionally
/// avoids pulling a datetime parser into its dependency tree; a malformed
/// literal panics with `"clapfig: invalid datetime literal in static
/// schema default"` at the first `Schema::schema()` call (see the datetime
/// caveat in the macro docs).
fn scalar_default_to_value_static(expr: &Expr, kind: ScalarKind) -> syn::Result<TokenStream2> {
    if let Expr::Array(_) = expr {
        return Err(syn::Error::new(
            expr.span(),
            "array-literal defaults are only valid on Vec<T> (or Option<Vec<T>>) fields",
        ));
    }
    if kind == ScalarKind::DateTime {
        if let Expr::Lit(ExprLit {
            lit: Lit::Str(s), ..
        }) = expr
        {
            let value = s.value();
            return Ok(quote! { ::clapfig::static_schema::ValueStatic::Datetime(#value) });
        }
        return Err(syn::Error::new(
            expr.span(),
            "datetime defaults must be string literals in TOML datetime syntax \
             (e.g. `default = \"2020-01-01T00:00:00Z\"`)",
        ));
    }
    value_static_from_expr_with_kind(expr, kind)
}

/// Permissive default for `#[clapfig(value)]` leaves: any literal, negated
/// numeric literal, or array literal of those.
fn value_default_to_value_static(expr: &Expr) -> syn::Result<TokenStream2> {
    match expr {
        Expr::Array(a) => {
            let items: Vec<TokenStream2> = a
                .elems
                .iter()
                .map(value_static_from_expr)
                .collect::<syn::Result<_>>()?;
            Ok(quote! {
                ::clapfig::static_schema::ValueStatic::Array(&[ #(#items),* ])
            })
        }
        other => value_static_from_expr(other),
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
            let neg = parse_signed_integer_literal(i, true)?;
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

fn parse_signed_integer_literal(lit: &syn::LitInt, negative: bool) -> syn::Result<i64> {
    if !negative {
        return lit
            .base10_parse()
            .map_err(|e| syn::Error::new(lit.span(), e));
    }
    let raw: u64 = lit
        .base10_parse()
        .map_err(|e| syn::Error::new(lit.span(), format!("integer literal: {e}")))?;
    let neg_i128 = -(raw as i128);
    i64::try_from(neg_i128).map_err(|_| {
        syn::Error::new(
            lit.span(),
            "negated integer literal exceeds the i64 range (TOML's integer width)",
        )
    })
}
