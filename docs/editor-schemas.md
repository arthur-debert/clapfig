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

## The directive is a comment

`#:schema …` is an ordinary TOML comment, so a generated file carrying it
parses, loads, and strict-validates exactly as the same file without it. You
can ship the directive in a template you seed users' config files from and
nothing downstream has to know about it.

## Proof

`bin/tombi-proof.sh` generates an
[edward](https://github.com/arthur-debert/edward)-shaped pair through the
`schema_directive` example and runs tombi against it: the generated template
validates, a filled-in block instance validates, and an unknown key is
rejected — the last one being the control that shows the directive is
actually resolved. tombi runs from a pinned `uvx` release, so the check needs
nothing installed globally.
