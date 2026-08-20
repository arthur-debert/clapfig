# Spec: shape algebra — the schema node is a shape, not always an object

## Context

Clapfig's schema is declared in Rust — `#[derive(clapfig::Schema)]` or a
runtime-built object — and every consumer walks that one tree: layered
resolve, strict unknown-key checks, defaults, type/enum validation, JSON
Schema export, template generation, metadata lookup, persistence. After
the value-model epic (`docs/spec/value-model.md`) the pipeline speaks
clapfig `Value`. After the provenance epic (`docs/spec/provenance.md`)
post-merge errors on a value that exists name the winning origin.

What the schema tree itself still is: a named-field **object**.
`runtime::Schema` is `{ name, doc, strict, fields }`. A `Field` is
`Leaf | Nested(Schema) | ArrayOf(Schema) | MapOf(Schema)`. The document
root is a `Schema`, so it is always a product of known names. Homogeneous
maps and arrays of objects exist only as *fields of an object*. Primitive
maps and arrays are a second encoding (`LeafType::Map` / `LeafType::Array`).
There is no union.

That model is enough for a typical app config (`host`, `port`, `[database]`).
It is not enough for configs whose interesting type is "a map of named
instances, each sharing some fields and differing by a discriminator" —
the shape Edward's `.edward/blocks.toml` and `.edward/artifacts.toml`
actually want. Issues #98–#103 were filed from those wishes. This Spec
does not implement those issues. It names the capability they are
instances of.

Constraints already decided, not reopened:

- Maps are unordered ([ADR-0001](../adr/0001-clapfig-owns-its-value-model.md),
  #99 closed as not planned). Declaration order is not a schema shape.
- Merge is key-wise deep merge; arrays replace wholesale
  (`docs/spec/provenance.md` Out Of Scope). Tagged-union *branch
  selection* runs after merge, on the winning tree, and uses the origin
  tree provenance already landed. Unknown-key checking on tagged
  objects is two-phase (true unknowns per-file pre-merge; branch-
  exclusive keys post-merge); merge itself does not change.
- Formats are adapters ([ADR-0002](../adr/0002-formats-are-adapters-with-declared-capabilities.md)).
  Schema composition is format-agnostic. TOML remains the baseline test
  for what a shape may mean.
- One schema model: the confique path is gone. Derive and runtime emit
  the same runtime tree. There is no "legacy static path."
- `#[clapfig(value)]` remains the honest escape hatch for shapes clapfig
  does not represent. Untagged / adjacent-tagging-as-a-primitive stay
  there until a later Spec says otherwise.
- Derive arrays of structs and of unit enums already work (DER01-WS03;
  the mechanism behind postponed #101 / #102). This Spec does not redo
  them; it generalizes the composition they sit in.
- The schema **node** is `Shape`; `runtime::Schema` stays the named-field
  object constructor ([ADR-0010](../adr/0010-shape-is-the-schema-node.md)).
  Walkers take `Shape`. That layout is not reopened here.

**Shape** is a schema node — leaf, object, homogeneous map, homogeneous
array, or tagged union — usable as a field, as a map entry, and as an
array item. Legal **document roots** are the object-shaped constructors:
Object, Map, and internally tagged objects. Leaf and Array as document
roots fail the TOML baseline (a TOML document is a table; load returns
`value::Map`; `resolve` rejects a non-map document). A tagged-union
variant is an object (`Schema` / the `Object` constructor), not an
arbitrary shape. Today's named-field object is one constructor, not the
node. Do not call a shape a "schema type" or a "field kind."

## Problem

Users declare polymorphism in Rust and get an untyped hole in the schema.

Edward's block instance is the concrete case:

```toml
[block.core]
kind = "rust"
mount = "."
params = { shape = "cli-plus-lib" }

[block.minsky]
kind = "payload"
params = { artifact = "...", entry = "minsky start" }
```

`kind` is an open `String`. `params` is `#[clapfig(value)]`. A second
serde pass, keyed on `kind`, checks the table. Unknown keys inside
`params` do not go through clapfig's unknown-key cascade. Type and
missing-field errors inside `params` are serde strings, not
`ClapfigError::InvalidValue` / `MissingRequired`. The exported JSON
Schema types `params` as "any value", so editor tooling cannot catch a
misspelled kind-specific key. `config gen` cannot document what a rust
block accepts.

Clapfig *almost* has the pieces. Named `MapOf` exists (#54): that is why
Edward wraps instances under `[block.<name>]` instead of writing `[core]`
at the root. `ArrayOf` exists. Unit enums exist. Provenance can locate a
post-merge error. The gap is composition: those constructors cannot be
the document root, cannot contain each other freely, and cannot express
"this object is one of a closed set of objects, selected by a
discriminator field."

Two encoding splits make the gap structural, not a missing derive arm:

1. **Root is always an object.** A homogeneous map of user-named tables
   cannot be the document. The workaround is a synthetic parent field
   (`block`, `artifact`) whose only job is to give the map a name.
2. **Item kind splits the constructor.** A map of strings is
   `LeafType::Map`; a map of objects is `Field::MapOf(Schema)`. The same
   split exists for arrays. There is no map of unions, no array of maps
   of objects, no union of maps. Adding "root may be a map" as a boolean
   on today's `Schema` would freeze the split rather than remove it.

The #100 Decision comment named the right instinct — a tagged union of
the *whole object*, not a callback that inspects a sibling to validate
`params` — then described the feature as "kind selects the schema of
params." That sibling-lookup special case is the Edward-shaped API this
work exists to avoid. Adjacent tagging (`{ kind, params }`) is a *use*
of an internally tagged union whose variants contain different nested
field types, not a clapfig primitive.

A related wish, #103 (editor schema directive on generated templates),
is real and orthogonal. It does not change the schema node. It is out of
scope here.

## Goals

1. **Shape is the schema node.** Every consumer that today walks
   `Schema` / `Field` walks a shape. The document root is a shape, and
   the legal roots are Object, Map, and internally tagged objects (not
   Leaf or Array). A shape is usable in every position a field's value
   is usable today (field, map entry, array item). Union-variant
   position is an object, not an arbitrary shape.
2. **Five constructors, composable.** Leaf (today's `LeafType`, including
   closed enums and the `Value` escape hatch). Object (today's `Schema`:
   named fields). Homogeneous Map (unordered string keys, item is a
   shape). Homogeneous Array (item is a shape). Tagged union (a
   discriminator field whose closed, unique, non-empty value set selects
   the rest of the object). The two map encodings and two array encodings
   collapse: a map of leaves and a map of objects are the same
   constructor with a different item shape.
3. **Root map.** A schema whose root is a homogeneous map loads, validates,
   exports JSON Schema (`type: object` + `additionalProperties: <item>`),
   and generates a template without inventing a parent field. The typed
   path accepts `BTreeMap<String, T>` / `HashMap<String, T>` where
   `T: Schema` as the document type. Dynamic-key limits on `config set` /
   overrides stay explicit, as they already are for named `MapOf`.
4. **Internally tagged union of the whole object**, matching serde's
   internally tagged enums: the tag is declared on the enum, it appears
   on the wire beside the variant's fields, and the variant type does
   **not** own a field of that name. Unknown discriminator →
   `InvalidValue` on the tag field, naming its origin. Missing
   discriminator → `MissingRequired` (no origin; discovery record, as
   today). Variant-specific unknown keys, missing fields, and type
   errors run against the *selected* variant and use the normal error
   model, including provenance.
5. **Post-merge branch selection.** Merge stays key-wise. Kind from one
   input type and variant fields from another is legal; validation sees
   the merged object and the winning origins. No new merge rule.
   Unknown-key checking is two-phase: true unknowns (not the tag, not a
   field of any variant) stay per-file / per-env pre-merge; branch-
   exclusive keys (a field of some other variant, not of the selected
   variant) run after branch selection on the merged object.
6. **Derive and runtime stay twins.** An internally tagged enum deriving
   `Schema` (via `#[serde(tag = "...")]`) emits the same runtime tagged
   shape the fluent builder can construct. A root-map typed load emits
   the same root-map shape. Untagged / `content` / adjacent serde
   representations remain derive-time errors naming `#[clapfig(value)]`.
7. **Honest artifacts.** JSON Schema describes tagged alternatives as
   `oneOf` branches with a `const` on the tag field. `config gen` emits
   a commented example of each variant, not one uncommented object that
   mixes keys from several variants.
8. **Existing object-root schemas keep their artifacts.** A schema that
   is a named-field object today produces the same JSON Schema, template,
   and load behavior unless it opts into a new constructor.

## Non-Goals

- Sibling-field schema selection ("validate `params` by looking at
  `kind`") as a clapfig primitive.
- Untagged, externally tagged, and adjacently tagged unions as shapes.
  `#[clapfig(value)]` plus caller-owned deserialize remains the escape
  hatch. A clapfig-only `#[clapfig(tag)]` that serde does not honor is
  not added: schema and deserialize would disagree.
- Adjacent tagging as a constructor. `{ kind, params: <varies> }` is
  internally tagged with a nested field whose type differs per variant.
- Declaration-order maps, ordered maps, or any walk that treats TOML
  source layout as meaning (#99, ADR-0001).
- Flatten, in any form. `#[serde(flatten)]` stays a derive-time error.
  Shared fields are written on each variant. No walker-visible flatten
  node and no derive expansion that inlines a base into variants.
- Changing merge semantics (including "the discriminator wins the
  variant before keys merge").
- Indexed or dynamic-key `config set` / override addressing
  (`plugins[0].id`, `blocks.core.shape` where `blocks` is a map). Today's
  refuse stays; making those keys addressable is a later Spec.
- Heterogeneous root objects that mix known properties with a
  *different-shaped* additional-properties map (Edward's `[repo]` beside
  user-named blocks). A product with a *named* `Map` field already
  expresses that. A pure root `Map` does not reserve a sibling.
- `#[serde(transparent)]` newtypes as a root-map spelling. The typed
  root *is* the map, or a named-field struct that contains one.
- Editor schema-document directives (`#:schema`, JSON `"$schema"`, YAML
  language-server comments). That is #103: format-adapter artifact
  generation, not shape composition.
- Re-implementing `Vec<UnitEnum>` / `Vec<NestedStruct>` / named `MapOf`.
  Those are in. This work generalizes the node they attach to.
- A public `Origin` type, `config list` origin annotations, or
  structured `post_validate` diagnostics (provenance follow-ons).
- OpenAPI `discriminator` (not JSON Schema). Portable JSON Schema is
  `oneOf` plus `const`.

## Proposed Shape

Five constructors. Every walker takes a shape. The document root is a
shape, but only object-shaped constructors are legal there: `Object`,
`Map`, and `Tagged`. `Leaf` and `Array` are nested shapes only — TOML
cannot represent a scalar or array document, and load still returns
`value::Map`. This Spec does not invent a root-scalar or root-array
return API. Today's named-field object is the `Object` constructor.

```text
Shape =
  | Leaf(LeafType)
  | Object { fields: [(name, Shape)], … }
  | Map   { item: Shape }
  | Array { item: Shape }
  | Tagged { tag: name, variants: [(discriminator, Object)] }
```

How those constructors sit in Rust (`Shape` enum, `Schema` remaining the
object, `Field` not a second node, map/array encodings collapsed, tagged
variants as `Schema` not `Shape`) is
[ADR-0010](../adr/0010-shape-is-the-schema-node.md). This section is
what users write, what files mean, and what artifacts look like.

### Leaf, object, map, array

**Leaf** is unchanged in meaning: primitives, closed enums, and `Value`.
Homogeneous leaf arrays/maps are `Array` / `Map` with a leaf item — the
same constructors as arrays/maps of objects. Leaf is not a legal
document root.

**Object** is today's named-field product. Absence rules stay: an absent
nested object is the empty table; `Option<NestedStruct>` is not a shape.
`Option` around a tagged object is the same rejection.

**Map** is homogeneous and unordered. Keys are user data. Item is any
shape, so a map of tagged objects is `Map { item: Tagged { … } }` and a
map of strings is `Map { item: Leaf(String) }`. An absent map is the
empty map, as today. JSON Schema: `type: object` with
`additionalProperties: <item>`. At the document root this is #98 with no
sentinel parent.

The typed root may be `BTreeMap<String, T>` or `HashMap<String, T>`
where `T: Schema`. A named-field struct that *contains* a map field
(`struct File { block: BTreeMap<String, Block> }`) already works and
stays the way to keep a reserved sibling (`repo`) next to user-named
entries. A `transparent` newtype wrapping a map is not added.

**Array** is homogeneous. Item is any shape. An absent array is the empty
array, as today. JSON Schema: `type: array` with `items: <item>`.
Array (like Leaf) is not a legal document root; nested use is
unchanged.

### Tagged unions

Internally tagged, the way serde's `#[serde(tag = "kind")]` already
means on the wire:

```rust
#[derive(Schema, Serialize, Deserialize)]
#[serde(tag = "kind")]
enum Block {
    #[serde(rename = "rust")]
    Rust { mount: String, params: RustParams },
    #[serde(rename = "payload")]
    Payload { mount: String, params: PayloadParams },
}
```

```toml
kind = "rust"
mount = "."
params = { shape = "cli-plus-lib" }
```

The tag name is declared on the enum (`serde(tag = ...)`). It is **not**
a field of the variant type. Derive that finds a variant field with the
same name as the tag is a derive-time error (schema and serde would
fight). The tag key is reserved on that object: required, closed (the
variant discriminators: unique, non-empty, at least one), never an
unknown key.

Variant discriminators follow the same rename rules as unit-enum
variants (`rename` / `rename_all` on serde or clapfig). The allowed set
is closed, unique, and non-empty: at least one variant; post-rename
discriminator values are unique and non-empty. That is the union
analogue of `LeafType::Enum`. An empty tagged union, an empty
discriminator spelling, or a post-rename collision is an authoring
error — derive-time for `#[derive(Schema)]`, construction-time for the
fluent builder. Tests lock `rename` and `rename_all` collisions the
same way unit-enum derive already does.

Each variant is an **object** (`Schema` / the `Object` constructor) of
the variant's own fields. The algebra writes
`variants: [(discriminator, Object)]` for that invariant: a variant is
not an arbitrary `Shape`. A unit variant is the empty object: the file
carries only the tag (`kind = "off"`). A variant that is itself a Map,
Array, Leaf, or Tagged is out of scope — that is a different serde
representation.

`params` as a nested table whose type differs per variant is still this
union: two objects that happen to contain different nested field types.
Flattening those fields onto the block (`shape = "cli-plus-lib"` beside
`kind`) is the same union with a flatter object per variant. Clapfig
does not care; Edward's file format does. Shared fields (`mount` on
every block) are written on each variant.

After merge, clapfig reads the tag, selects the variant, and validates
the rest of the object against that variant's fields. Merge stays
key-wise; branch selection does not change which keys survive.

**Unknown keys on tagged objects, two phases.** Provenance keeps
unknown-key validation per-file (and per env table) *pre-merge*. Tagged
unions also need a selected branch, which exists only after merge. Both
are true; neither is waived. Merge semantics do not change.

1. **Pre-merge (each file and env layer, as today).** Treat the tagged
   object as the union of the tag key and every variant's fields. A key
   that is the tag, or a field of *any* variant, is known at this phase.
   A key that is none of those is a true unknown: run it through the
   existing strictness cascade / `on_unknown_key` callback / `Collect`,
   using that layer's own spans (file) or env-var names — not the merged
   origin tree. This phase does **not** read that layer's discriminator
   to select a branch. A sparse layer that supplies rust-only keys and
   no `kind` is legal here; a sparse layer that supplies
   `totally_unknown` is not.

2. **Post-merge (after branch selection).** Read the winning
   discriminator (winner origin of the tag). Select that variant. The
   candidate set is **branch-exclusive keys only**: keys that belong to
   at least one *non-selected* variant and not to the selected variant
   (and are not the tag). True unknowns — keys that are not the tag and
   not a field of any variant — are phase 1 only. They are not re-run
   through the cascade, callback, or `Collect` here, even if they
   survived into the merged tree (`Accept`, `Collect`, or a lenient
   ancestor). A true unknown is processed only in phase 1:
   cascade-lenient keys invoke no callback; `Accept` invokes it once
   without collection; `Collect` invokes it once and appends at most
   one entry; `Reject` invokes it once and fails the load.

   For each branch-exclusive key, apply the same strictness cascade,
   callback, and `Collect` path. Locations come from the merged origin
   tree (winner-only). An overwritten lower-layer key reports the
   winner's origin, never a loser; a key that survived from a lower
   layer reports that layer.

Strictness lookup is the existing cascade: nearest ancestor schema node
with an explicit `strict`, else the builder default. The tagged object
is that parent. Keys the cascade marks lenient never reach the
callback; cascade-strict keys go through `on_unknown_key` (`Reject` /
`Accept` / `Collect`) the same way a named-field object's unknown keys
do today. `Collect` entries from either phase append to the same
`load_with_unknowns` list. A given key is a candidate of at most one
phase, so it is never collected twice. Cascade-lenient keys never
reach the callback in either phase.

**Layered discriminator.** File A supplies `kind = "rust"`. File B
supplies rust-only keys. An env var later sets `kind = "payload"`. Merge
produces a payload-kind object that may still contain rust-only keys.
Phase 1 accepts those rust-only keys on each layer (they belong to some
variant). Phase 2 reports them as unknown against the payload variant,
each naming its winning origin. That is the honest result of key-wise
merge plus post-merge branch selection; clapfig does not rewind the
merge.

**Derive.** Internally tagged `Schema` enums honor `#[serde(tag = "...")]`
— the same policy already used for `rename` / `rename_all`. Inventing
`#[clapfig(tag)]` without serde honoring it would accept a file serde
then fails to deserialize, which is the class of divergence derive
hardening rejects. `untagged`, `content`, and adjacent tagging stay
derive-time errors naming `#[clapfig(value)]`. `flatten` stays a
derive-time error.

**Runtime.** The fluent builder constructs the same tagged shape derive
emits, so a host assembling a schema at startup is not a second
language.

### JSON Schema

Object, Map, Array, and Leaf keep today's mapping (including the
ADR-0002 comment-key allowlist and `additionalProperties: false` on
closed objects). A root map is `type: object` with
`additionalProperties` of the item schema at the document root.

A tagged union is `oneOf`. Each branch is that variant's object schema
plus the tag field as a required property whose schema is
`{ "const": "<discriminator>" }` (and typically `"type": "string"`).
Branches are distinguishable by the tag constant. OpenAPI's
`discriminator` keyword is not used.

### Templates

`config gen` for a tagged shape emits **one commented example per
variant**, each a complete object for that discriminator (tag plus that
variant's fields). The user uncomments the one they want. It does not
emit a single uncommented object that is valid for no variant.

A root map's template shows a commented example entry, not an invented
parent table. Object-root templates that do not use new constructors
stay byte-identical to today.

### Persistence

Keys whose path segment is user data (map entries) or variant-specific
given an unset or conflicting discriminator keep today's targeted
refuse. This epic does not invent addressing for them.

### Public surface

Widening the schema enums is a hard cut with a changelog migration note,
per project policy. Object-root callers that do not use the new
constructors should see unchanged load and artifacts.

## User / Agent Stories

1. As an **app developer**, I want the document root to be a map of
   named instances (`[core]`, `[site]`), so I do not invent a parent
   field just to give the map a home.
2. As an **app developer**, I want to declare `#[serde(tag = "kind")]`
   on a `Schema` enum and have clapfig validate that file, so a rust
   block and a payload block are different objects with a shared
   discriminator, not an untyped `params` hole — and serde deserialize
   of the same tree succeeds.
3. As an **end user**, I want a bad key on a rust block to fail the way
   a bad key on `[database]` fails — clapfig error, file, key, line —
   including when `kind` came from the file and the bad key came from
   env, or the reverse.
4. As an **end user**, I want an unknown `kind = "rus"` to name the
   origin of `rus` and the allowed set, the way a bad unit-enum leaf
   already does.
5. As an **editor user**, I want the exported JSON Schema `oneOf`
   branches to describe each variant's keys (and a root map's entry
   schema), so completion and validation as I type match load-time
   checks.
6. As an **app developer**, I want `config gen` for a tagged schema to
   give me a commented example of each variant I can uncomment, not one
   uncommented blob that is illegal for every variant.
7. As a **runtime-schema host**, I want to build the same tagged and
   root-map shapes with the fluent builder that derive emits, so plugin
   schemas assembled at startup are not a second, weaker language.
8. As a **clapfig contributor or coding agent**, I want one map
   constructor and one array constructor whose item is a shape, so I
   stop inventing `LeafType::Map` vs `Field::MapOf` special cases when
   composing.
9. As an **existing object-root user**, I want this change invisible
   unless I opt into a new constructor: same resolution, same generated
   artifacts.

## Risks And Rabbit Holes

- **Implementing #98 as a root flag on today's `Schema`.** "Object or
  map at the top, everything below unchanged" looks small and ships a
  second special case. The walkers then still cannot put a tagged union
  in a map entry. Resist. Root map falls out of Map being a legal
  document root.
- **Root Leaf or Array.** Tempting once Shape is the node. TOML cannot
  be a scalar or array document; load returns `value::Map`. This Spec
  does not invent a root-value return API. Nested Leaf and Array stay.
- **Sibling-lookup for `params`.** Faster to demo on Edward's current
  file, and it is the example the #100 Decision comment still writes.
  It does not compose (nested discriminators, unions as map items) and
  it is not what internally tagged serde enums are.
- **`#[clapfig(tag)]` without serde.** Looks like clapfig-native
  spelling. The load would accept a file the typed deserialize then
  rejects (or the reverse). Honor `serde(tag)` so one attribute is the
  contract.
- **Flatten as expansion or as a walker node.** Convenient for "base +
  extension" in Rust; this epic duplicates shared fields on each
  variant. Revisit later, as its own Spec, if authoring pain is real.
- **Edward `blocks.toml` as the root-map proving case.** That file is a
  named map *plus* a reserved `[repo]` sibling of a different shape. A
  product `{ block: Map<Block>, repo: Repo }` already works. A pure root
  `Map<Block>` does not have a place for `repo`. `artifacts.toml` (no
  reserved sibling) is the honest root-map example. Mixed
  properties + additionalProperties of a different item shape is a
  follow-on, not this epic.
- **Untagged / externally tagged / adjacently tagged serde enums.**
  Serde has four representations. This Spec takes one (internally
  tagged). Honoring the others "while we're here" reopens the
  ambiguous-match error quality that `LeafType::Value` exists to avoid.
- **Templates that invent a combined object.** The easy `config gen`
  implementation walks every field of every variant into one table.
  That artifact is valid for nobody and teaches the wrong file.
- **Merge-aware unions.** Tempting to drop keys that belong to the
  losing kind when the discriminator changes across layers. That is a
  merge-semantics change, out of scope, and silent data loss.
- **Addressing dynamic keys in `config set`.** In scope to *refuse
  clearly*. Out of scope to make `blocks.core.shape` work for a root
  map. Scope creep here is a second epic.
- **Public enum widening without a changelog note.** `Field` /
  `FieldStatic` / builder methods are public. Hard cut is policy; the
  migration note is still required.
- **Optional tagged unions.** `Option` around nested objects and around
  `MapOf` is already rejected; absence is the empty container. A tagged
  object in field position follows the same rule: absent → empty table,
  then `MissingRequired` on the tag. Do not invent a third absence rule.

## Cross-Cutting Concerns

- **Compatibility.** Schema-node widening is a hard cut. Object-root
  behavior is regression-locked. Callers matching on `Field` exhaustively
  (the enum is not `non_exhaustive` today) will not compile; that is
  the point of the changelog note. No deprecation window.
- **Observability.** No new tracing doctrine, and no exemption for
  discriminators. Branch selection is a post-merge validation step:
  `trace` records that selection happened, naming the discriminator
  *path*, the winning origin of that path, and the value *type*
  (string). It does not emit the discriminator string or the selected
  variant name — that name is the user's resolved value. User-facing
  errors may still quote the offending discriminator (`InvalidValue`
  naming `"rus"`), per the provenance two-contract rule. Tracing events
  never contain config values (`docs/spec/provenance.md`; ADR-0009).
- **Provenance.** Already landed. Tagged-union and root-map errors must
  go through `InvalidValue` / `MissingRequired` / unknown-key so they
  inherit origin and discovery. A second pass that only has serde
  strings is a regression of the #100/#101/#102 qualification provenance
  closed.
- **JSON Schema / editors.** Export is `oneOf` + `const` on the tag.
  Binding the generated file to an editor via a format-specific
  directive is #103, not this Spec.
- **Performance.** Shape trees are small (they describe the schema, not
  the data). Branch selection is one discriminator read per tagged node
  after merge. No new retained structure beyond the schema itself.
- **CI/release.** Changelog fragment with the constructor list, the
  derive spelling (`serde(tag)`, map as typed root), and the
  "object-root artifacts unchanged" guarantee.

## Testing / Verification

Signature evidence, all automated. Edward is motivation, not a clapfig
CI fixture.

- **Root map, runtime and derive.** A schema whose root is
  `Map { item: Object { … } }` loads `[core]` / `[site]` with no parent
  field; `Clapfig::typed::<BTreeMap<String, T>>()` (and `HashMap`)
  produces the same; unknown keys, missing required fields, and type
  errors name origin; JSON Schema is `additionalProperties` of the item
  at the *root* (not under a synthetic property); template has no
  invented parent table. `config set` of a dynamic entry key refuses
  with the existing class of error.
- **Illegal document roots.** A schema whose root is Leaf or Array is
  rejected (derive-time for a typed root that is a scalar or `Vec<T>`;
  runtime-construction for a root Leaf/Array shape). No new return type
  is added. Nested Leaf and Array keep working as field values, map
  items, and array items.
- **Tagged union, runtime and derive.** `#[serde(tag = "kind")]` two
  variant structs: good rust instance loads and typed-deserializes;
  unknown discriminator is `InvalidValue` on the tag field with origin
  and allowed set; missing tag is `MissingRequired` with discovery, not
  an origin; a payload-only key on a rust instance is an unknown-key
  error located on that key. A unit variant loads as tag-only. Variant
  payloads are objects: constructing a tagged shape whose variant is
  Map, Array, Leaf, or Tagged is a construction error.
- **Discriminator authoring.** Empty tagged union, empty discriminator
  spelling, and post-rename collisions (`rename` two variants onto the
  same string; `rename_all` mapping two variants onto the same string)
  are derive-time errors and runtime-construction errors. UI fixtures
  lock the derive messages, matching unit-enum derive.
- **Tag/field clash.** A variant field named the same as `serde(tag)`
  is a derive-time error. UI fixture locks the message.
- **Sparse layers, both directions.** Discriminator from file, variant
  field from env, and the reverse. A file or env layer that supplies a
  variant-specific key *without* a discriminator on that layer is legal
  at phase 1 and validated against the selected variant at phase 2. A
  true unknown (`not_a_field_of_any_variant`) on a sparse file layer is
  rejected pre-merge, even with no discriminator on that layer; the
  same for a sparse env layer. A true unknown that survives phase 1
  (`Accept`, `Collect`, or cascade-lenient) is not a phase-2 candidate.
  Phase-1 outcomes, asserted separately: cascade-lenient invokes no
  callback; `Accept` invokes once without collection; `Collect` invokes
  once and appends one entry; `Reject` fails the load at phase 1 with
  one invocation. Discriminator from env *changing* the kind
  while the other layer still supplies the old kind's keys: phase 1
  accepts those keys (they belong to some variant); phase 2 reports
  them as unknown against the winning kind, origins on the losing keys
  (winner-only: an overwritten key names the winner, not the loser).
  Callback / `Collect` / cascade-lenient behavior is covered in both
  phases and both file→env and env→file directions.
- **Composition.** `Map { item: Tagged { … } }` (the Edward-shaped
  config without a parent field, or with one — both). `Array { item:
  Tagged { … } }`. Nested tagged objects. Each path through unknown-key,
  defaults, finalize, JSON Schema.
- **Map/array collapse.** A map of unit enums and a map of objects go
  through the same map constructor; a `Vec<UnitEnum>` and a
  `Vec<NestedStruct>` through the same array constructor. Existing
  DER01-WS03 tests keep passing; they are the regression lock for those
  item kinds.
- **JSON Schema.** Root map; tagged `oneOf` with per-branch tag
  `const`; map of tagged; object-root unchanged (including ADR-0002
  `^//` comment-key allowlist and `additionalProperties: false` on
  closed objects). No OpenAPI `discriminator` key.
- **Templates.** Tagged schema: one commented example per variant; no
  single uncommented mixed-variant object. Object-root templates
  byte-identical to today when the schema does not use new constructors.
- **Derive diagnostics.** `untagged` / `content` / adjacent serde
  attributes remain derive-time errors naming `#[clapfig(value)]`.
  `flatten` remains a derive-time error. Internally tagged is accepted.
  UI trybuild fixtures lock the messages.
- **Provenance lock.** Every new `InvalidValue` in the tests above
  carries origin facts; no new error path that only has a dotted key.
- **Tracing lock.** Branch-selection `trace` events name discriminator
  path, origin, and value type; they do not contain the discriminator
  string or the selected variant name.

Prior art to copy: `crates/clapfig/tests/derive_array_of.rs` and the
MapOf coverage in `derive_schema.rs` / `runtime.rs` builder tests for
container absence and per-entry unknown keys; `schema_walk.rs` unit
tests for finalize/defaults; `json_schema.rs` tests for
`additionalProperties`; provenance post-merge error tests for origin
on `InvalidValue`; `tests/ui/derive/serde_container_attrs_rejected.rs`
for the reject-loudly serde policy.

## Workstream Hints

Interface-first, same reason as the value-model contract: several later
slices walk the same node, and getting the node wrong rewrites all of
them.

- **The contract:** the shape node (runtime + static mirror + builder
  methods + `Schema` trait root), signatures only where behavior is
  pending; existing object-root schemas still construct; data-model
  tests (construction, "root may be a map", "Leaf and Array are not
  legal document roots", "tagged has a closed unique non-empty
  discriminator set", "tagged variants are objects").
  Architecture-reviewable in isolation.
  [ADR-0010](../adr/0010-shape-is-the-schema-node.md) is this node's
  layout.
- **Walk:** `schema_walk` (unknown keys, defaults, finalize) and the
  strictness cascade treat Map/Array/Tagged at any depth. Map and Tagged
  also at document root; Array only nested. Demoable as "root map
  unknown key is located" and "wrong variant field is an unknown key
  against the selected kind" (two-phase: true unknowns pre-merge,
  branch-exclusive keys post-merge; a surviving true unknown is not
  re-collected).
- **Derive:** `serde(tag)` enums; `BTreeMap<String, T>` / `HashMap` as
  the typed root. Demoable as Edward-shaped Rust types loading without
  `#[clapfig(value)]` on `params` *when those params are fields of the
  variant*.
- **JSON Schema + templates:** `oneOf` + `const`; commented per-variant
  gen. Can proceed once the walk accepts the shapes, in parallel with
  each other.

Do not slice as "root maps" then "tagged unions" as two object-model
special cases. Do not slice as "core shape logic" then "I/O" then
"render."

Issue 103 (schema directive) is a separate, parallel change if picked
up; it does not share this contract.

## Out Of Scope

- #99 (map declaration order) — closed, not planned.
- #101 / #102 as implementation work — already landed; close the issues.
- #103 editor-discovery directives.
- Untagged, externally tagged, and adjacently tagged unions as shapes.
- Flatten (`serde(flatten)` stays rejected; no expansion).
- `#[clapfig(tag)]` as a serde-independent spelling.
- `#[serde(transparent)]` newtypes for root maps.
- Merge-semantics changes.
- Root Leaf or Array (no new load return API; nested use only).
- Dynamic-key and indexed-path persistence addressing.
- Heterogeneous root (known fields + additionalProperties of a different
  shape).
- OpenAPI `discriminator`.
- Any format beyond TOML / YAML / JSON.
- Promoting `Origin` to public.

## Further Notes

- User wishes this Spec generalizes: #98 (root map), #100 (tagged
  union). #99 is already closed. #101 / #102 are done (DER01-WS03) and
  their provenance qualifier is done (PRV01). #103 is a sibling concern,
  not a shape constructor.
- The #100 Decision comment remains useful as a rejected approach
  (sibling-lookup) and as a checklist of surfaces (defaults, strictness,
  metadata, persist, templates, JSON Schema, provenance). Provenance
  item (6) on that list has landed; the rest is this Spec's tagged-union
  contract (internally tagged whole object, not sibling lookup).
- Prior ADRs this Spec consumes:
  - [ADR-0001 — Clapfig owns its value model](../adr/0001-clapfig-owns-its-value-model.md)
    (unordered maps; baseline test).
  - [ADR-0002 — Serialization formats are adapters with declared capabilities](../adr/0002-formats-are-adapters-with-declared-capabilities.md)
  - [ADR-0004 — Origin data travels as a shadow tree](../adr/0004-origin-data-travels-as-a-shadow-tree.md)
- ADRs from the grill of this Spec:
  - [ADR-0010 — The schema node is Shape; Schema stays the object constructor](../adr/0010-shape-is-the-schema-node.md)
- Sequencing named in `docs/spec/provenance.md`: provenance before
  tagged unions; shape-algebra (root maps #98) a separate successor.
  That successor is this Spec. Provenance is merged (`main`, PR #162).
- Follow-on hooks, not this Spec: properties + additionalProperties of
  a different item shape; addressing map-entry keys in `config set`;
  #103 format-adapter schema directives; `config list` origin
  annotations; flatten-as-derive-expansion if authoring pain shows up.
