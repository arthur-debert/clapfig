# Editor-discoverable config files

A user editing your app's config file can get completion, hover docs, and
live validation — if their editor can find the JSON Schema that describes
the file. TOML language servers (tombi and the tools that follow its
convention) find it by reading a directive on the file's first line:

```toml
#:schema ./blocks.schema.json

[block.core]
kind = "rust"
```

Clapfig generates both halves of that pair — the config template and the
JSON Schema it points at — from one schema, through `artifacts()`.

## Generating the pair

```rust
use clapfig::Clapfig;
use clapfig::artifacts::{ArtifactOptions, SchemaReference};

let options = ArtifactOptions::new()
    .schema_reference(SchemaReference::new("./blocks.schema.json")?);

let pair = Clapfig::typed::<BlocksFile>()
    .app_name("edward")
    .artifacts(&options)?;

std::fs::create_dir_all(".edward")?;
std::fs::write(".edward/blocks.toml", &pair.template)?;
std::fs::write(".edward/blocks.schema.json", &pair.schema)?;
```

`pair.template` is the same body `config gen` renders, with the directive as
its first line and a blank line between it and the body. `pair.schema` is the
same JSON Schema text `config schema` emits. Both entry points have the
method — `Clapfig::typed::<C>()` and `Clapfig::builder(shape)` — and both
generate from the one `Shape` the builder holds, so the schema document
describes the template shipped beside it.

Leave the reference out (`ArtifactOptions::new()`) and you get the two
artifacts with no directive: the template is byte-for-byte `config gen`
output. That is the opt-out, and it is the default.

## Who owns what

**Clapfig** generates the two contents from one schema.

**You** own everything about identity and deployment:

- which relative path or URL the reference is,
- where the two files live and under what names,
- writing them (clapfig writes nothing — it returns strings),
- publishing and versioning the schema document.

Clapfig never derives a reference from an output path, never resolves or
rewrites the reference, and never checks that it names a reachable document.
The two files agree because they were generated together; they keep agreeing
only for as long as you keep them together. Move one, hand-edit one, or
regenerate one alone, and nothing in clapfig notices.

## The reference is opaque, but it is one line

`SchemaReference::new` accepts any single-line text — a relative path, an
absolute path, an `https://` URL — and passes it into the directive verbatim.
It rejects what the directive's shape cannot carry: an empty or
whitespace-only value, a line break (which would end the directive and leave
the rest as config source), any other control character, and leading or
trailing whitespace (clapfig does not silently trim your reference). A
rejected value is `ClapfigError::InvalidSchemaReference`.

## Which formats have a directive

The directive is format syntax, so the format adapter spells it
(`Operation::SchemaDirective` in ADR-0002's capability matrix). TOML declares
it and writes `#:schema <reference>`. YAML and JSON declare no directive:
asking for artifacts with a reference under either refuses with the typed
`UnsupportedByFormat` error rather than quietly dropping the binding. Without
a reference, every format generates the pair.

The template is rendered in the builder's **preferred format** (the first
entry of `formats(...)`, TOML unless you enable others) — the same rule
`config gen` follows when it writes to stdout.

## Key spelling follows `normalize_keys`

With `.normalize_keys(true)`, the template renders keys and section headers
in kebab-case (`pool-size`, `[my-section]`) — the spelling clapfig *writes*.
What it *reads* is wider: that builder rewrites `-` to `_` on the way in, so
it loads `pool-size` and `pool_size` alike. The JSON Schema describes the
wider set, naming each multiword key twice:

```json
"properties": {
  "pool-size": { "type": "integer", "default": 5 },
  "pool_size": { "type": "integer", "default": 5 }
},
"allOf": [
  { "not": { "required": ["pool-size", "pool_size"] } }
]
```

Both names carry the same subschema, so the editor gives the same type, doc,
and default whichever one is typed. The rule beside them keeps the pair
honest: a key may appear under **at most one** spelling, which is how the
loader behaves — a document holding both is a collision it refuses rather
than picking a winner by key order. A *required* multiword key uses `oneOf`
over the two spellings instead, since it must appear under exactly one.

Naming a single spelling would not do. Object schemas here are closed
(`additionalProperties: false`), so a `pool_size`-only document makes the
editor flag every key in the kebab template it is bound to, and a
`pool-size`-only document flags a snake_case config file a user wrote by
hand — one clapfig loads without complaint.

Tagged-union tag keys and variant fields are keys, and get the same pair of
names; discriminator *values* are left alone, as are doc comments and
defaults. Single-word keys have nothing to alias and appear once.

Two names, not every name: a key of three or more words also loads
hand-mixed (`max-retry_count`), and the schema does not describe that.
Spelling out the combinations would multiply such a key's properties by
`2^(words - 1)`, and an editor's completion list with them, to describe a
form neither clapfig nor any convention writes.

`config schema` on its own emits that same document, so the standalone action
and `artifacts()` never describe the config file differently.

## The directive is a comment

`#:schema …` is an ordinary TOML comment, so a generated file carrying it
parses, loads, and strict-validates exactly as the same file without it. You
can ship the directive in a template you seed users' config files from and
nothing downstream has to know about it.

## Proof

`bin/tombi-proof.sh` generates an
[edward](https://github.com/arthur-debert/edward)-shaped pair through the
`schema_directive` example and runs tombi against it: the generated template
validates, a filled-in block instance validates under either spelling of its
multiword key, both spellings at once are rejected, and an unknown key is
rejected — the last one being the control that shows the directive is
actually resolved. tombi runs from a pinned `uvx` release, so the check needs
nothing installed globally.
