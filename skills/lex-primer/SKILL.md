---
name: lex-primer
description: |
  Primer for writing correct Lex documents. Use when:
  (1) Writing or generating .lex content
  (2) Understanding Lex syntax (it is NOT Markdown)
  (3) Reviewing whether a .lex file is syntactically correct
  (4) Converting content to Lex format
---

# Lex Document Primer

Lex is NOT Markdown. It uses indentation and conventions from publishing — no `#` headers, no `---` separators, no `**bold**`. If you write Markdown syntax in a .lex file, it will be treated as plain text.

## Core Principle

Structure = indentation (4 spaces per level). The only explicit syntax marker is `::` (for annotations/verbatim blocks). Everything else is determined by position, indentation, and punctuation patterns.

## The Seven Elements

### 1. Paragraph (fallback)

Consecutive non-blank lines. If nothing else matches, it's a paragraph.

```text
This is a paragraph.
It can span multiple lines.
```

### 2. Session (heading + content)

A title line, then a **blank line**, then **indented** content.

```text
1. Introduction

    This paragraph is inside the "Introduction" session.

    1.1. Background

        Nested session with its own content.
```

- Title can have an ordered marker (`1.`, `a.`, `I.`) or be plain text
- The blank line after the title is MANDATORY (distinguishes from definition)
- Content MUST be indented relative to the title

### 3. Definition (term + immediate content)

A subject line ending with `:`, then **immediately** indented content (NO blank line).

```text
HTTP Methods:
    GET retrieves resources.
    POST creates new resources.
```

- NO blank line between subject and content (that would make it a session)
- Content can include paragraphs, lists, and nested definitions
- Cannot contain sessions

### 4. List (2+ items after blank line)

Two or more list-item lines preceded by a blank line.

```text
Some intro text.

- First item
- Second item
- Third item
```

- Markers: `-` (dash), `1.` or `1)` (numbered), `a.` or `a)` (alpha), `I.` (roman)
- MUST have at least 2 items (single item = paragraph)
- MUST be preceded by a blank line (or document start)
- NO blank lines between items (blank line terminates the list)
- Marker style of first item defines the list style; mixing markers is allowed

### 5. Verbatim Block (raw content)

A subject line ending with `:`, optional blank line, indented raw content, and a closing `::` annotation.

```text
Example code:

    fn main() {
        println!("Hello");
    }

:: rust ::
```

- Content is NOT parsed (preserves raw text exactly)
- Closing `:: label ::` annotation is mandatory
- Marker form (no content): just subject + closing annotation

### 6. Annotation (metadata)

Structured metadata using `::` markers.

```text
:: note ::
:: warning status=open :: This needs review
:: aside ::
    Block content here.
    - Can include lists
::
```

Three forms:

- **Marker**: `:: label ::` (no content)
- **Single-line**: `:: label :: inline text`
- **Block**: `:: label ::` + newline + indented content + bare `::` closing

### 7. Table (subject + pipe rows)

A subject line ending with `:`, then an **indented** block whose first non-blank line is a pipe row (starts and ends with `|`).

```text
Quarterly Results:
    | Region | Revenue | Growth |
    | North  | $1.2M   | +12%   |
    | South  | $0.9M   | +4%    |
```

- Subject line ends with `:` (like a definition); its text is inline-parsed as the caption
- Every row needs leading AND trailing pipes; cells are split on `|`
- First row is the header by default
- Cell content supports all inlines (`*bold*`, `_italic_`, backtick-code, `[refs]`)
- Pipes need not visually align — the formatter aligns them; both forms are equivalent
- Markdown-style separator rows (`|----|----|`) are accepted and ignored (eases migration)
- Spanning uses `>>` (colspan) and `^^` (rowspan) in the absorbed cell

Organizational hints — alignment and header-row count — ride on an optional
`:: table ... ::` annotation **inside** the block, after the rows:

```text
Quarterly Results:
    | Region | Revenue | Growth |
    | North  | $1.2M   | +12%   |
    | South  | $0.9M   | +4%    |

    :: table align=lrr header=1 ::
```

- `align=lrr` — one letter per column: `l` left (default), `c` center, `r` right
- `header=N` — number of leading header rows (`header=0` for none); default `1`
- `table` is the blessed shortcut for the canonical `lex.tabular.table` label

There is only **one** table type. Markdown pipe tables convert to and from
native lex tables; the canonical `lex.tabular.table` label is the same element
in its historical verbatim spelling.

## Inline Formatting

```text
*bold text*           — strong (NOT **double asterisk**)
_italic text_         — emphasis (NOT *single asterisk*)
`code`                — inline code
#math expression#     — math notation
[reference]           — reference/link
```

Reference types (determined by content):

- `[https://example.com]` — URL
- `[@doe2024]` — citation
- `[^note1]` — footnote (labeled)
- `[42]` — footnote (numbered)
- `[#2.1]` — session reference
- `[./path/to/file]` — file reference
- `[TK]` — placeholder (to come)

Escape with backslash: `\*not bold\*`, `\[not a link\]`

## Common Mistakes (Lex ≠ Markdown)

| Wrong (Markdown) | Right (Lex) |
|---|---|
| `# Heading` | `1. Heading` + blank line + indented content |
| `**bold**` | `*bold*` |
| `*italic*` | `_italic_` |
| `[text](url)` | `[url]` (or footnote pattern) |
| ` ```code``` ` | Subject line + indented code + `:: lang ::` |
| `---` separator | blank line |
| `> blockquote` | indented paragraph in a definition/session |

## Indentation Rules

- 1 level = 4 spaces (tabs converted to 4 spaces)
- Partial indentation (e.g., 6 spaces = 1 indent + 2 literal spaces) is tolerated
- Content must be indented deeper than its parent element
- Dedent returns to parent scope

## Parse Precedence

The parser tries elements in this order:

1. Verbatim block / Table (subject + indented block with a closing annotation;
   a Table when the rows are pipe lines, otherwise a Verbatim block)
2. Annotation (`::` markers)
3. List (blank line + 2+ items)
4. Definition (subject + immediate indent)
5. Session (title + blank line + indent)
6. Paragraph (everything else)

## Checking Your Work

After writing or generating a `.lex` file, lint it with `lexd check` — the
checker that parses each document, runs the analysis pass, and reports
diagnostics with a CI-friendly exit-code contract.

```sh
lexd check doc.lex              # lint one (or more) files
lexd check *.lex --references   # also validate internal cross-references
lexd check doc.lex --format json   # machine-readable findings
```

Exit codes make it scriptable:

- `0` — clean (no findings at/above the `--fail-on` threshold, default `warning`)
- `1` — at least one finding met the threshold
- `2` — operational error (unreadable file, bad arguments)

Notes:

- `lex.include` annotations are expanded before checking (use `--no-includes`
  to skip); findings inside an included file are blamed on that file's path.
- `--references` additionally flags references whose target is absent from the
  whole (merged) tree — sessions, definitions, annotations, citations.
- Tune severity with `--fail-on error|warning|info|hint` and per-rule overrides
  in `[diagnostics.rules]` of `.lex.toml`.
