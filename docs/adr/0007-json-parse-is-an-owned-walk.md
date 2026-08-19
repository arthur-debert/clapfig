# JSON parse is a clapfig-owned walk

ADR-0005 requires `parse` to return the `Value` tree and the span index
together. `serde_json` has no byte offsets. We decided JSON **parse** is
a clapfig-owned walk that emits both in one pass, applying the baseline
rules that parse already applies (strip `//` keys, refuse null, integer
range). `serde_json` stays for serialize and edit (order-preserving
pretty-print, comments-as-data).

A span-aware JSON crate would be a second parser ecosystem while
`serde_json` remained for edit — ADR-0003 all over again. Locating keys
after a `serde_json` parse is the two-walk desync ADR-0005 forbids, and
`yamlpath` on JSON is the same mole plus YAML-vs-JSON grammar.

JSON config is the TOML-baseline subset; parse was already not vanilla
`serde_json`. The owned walk is the one-parse contract. It does not land
in the signature workstream: WS01 leaves an empty index; the JSON adapter
slice fills it.
