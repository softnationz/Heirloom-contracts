# Automated Documentation Generation

This document describes the documentation generator implemented in
`backend/src/doc_generator.rs`. It replaces hand-maintained reference docs,
which drift as soon as a signature or parameter name changes, with markdown
generated straight from the source.

## Why

Reference documentation written by hand goes stale silently: a renamed
parameter, a changed return type, or a deleted function leaves the prose
describing code that no longer exists. Generating the reference from source
means the docs can be regenerated on every change and diffed in CI, so drift
becomes a visible failure instead of an invisible one.

## Concepts

- **Item** (`DocItem`) - one public item, with its `ItemKind` (`function`,
  `struct`, `enum`, `trait`, `type`), a single-line `signature`, the `///`
  doc lines, extracted `params`, `returns`, and any attached `examples`.
- **Module doc** (`ModuleDoc`) - the module-level `//!` summary plus every
  public item found in the file, in source order.
- **Parameter** (`ParamDoc`) - a `name` and its declared `ty`, split at the
  binding colon so generics and paths (`Arc<State>`, `chrono::Utc`) survive.
- **Example** (`Example`) - a test body lifted verbatim from a `#[test]` or
  `#[tokio::test]` function, keyed by the test name so readers can find it.

Parsing is lexical rather than AST-based. Only item headers, doc comments,
and test bodies are needed, so a text scan keeps the generator free of any
syntax-tree dependency.

## API

### `parse_module(module, source) -> ModuleDoc`

Scans source text and returns the documentation model. Behavior worth
knowing:

- Only `pub` items are documented. `pub(crate)` and private items are
  treated as internal.
- Attributes between a doc comment and its item (`#[derive(...)]`,
  `#[allow(...)]`) do not detach the doc comment from the item.
- Multi-line signatures are collected until the body brace or the
  declaration semicolon, then normalized to one line.

### `extract_examples(source) -> Vec<Example>`

Returns one example per test function, with the body dedented to its own
indentation. Both `#[test]` and attribute-qualified async tests
(`#[tokio::test]`) are recognized.

### `attach_examples(&mut ModuleDoc, &[Example])`

Attaches examples to items by name containment: a test named
`test_evaluate_flag_returns_true` is attached to `evaluate_flag`. Items with
no matching test simply carry no examples.

### `render_markdown(&ModuleDoc) -> String`

Renders the model as markdown: a module heading and summary, then one
section per item with a fenced signature, doc prose, a parameter list, the
return type, and any examples.

### `generate(module, source, test_source) -> String`

One-call convenience: parse, extract examples from the test source when
provided, attach, and render.

## Generation process

The generator is a library function, so the process is a short driver that
reads sources and writes markdown:

```rust
use heirloom_protocol_backend::doc_generator::generate;

let source = std::fs::read_to_string("backend/src/feature_flags.rs")?;
let markdown = generate("feature_flags", &source, Some(&source));
std::fs::write("docs/generated/feature_flags.md", markdown)?;
```

Tests live in the same file as the code they exercise across this backend,
so the same text is normally passed as both `source` and `test_source`.

Recommended workflow:

1. Regenerate the reference for every module being changed.
2. Commit the generated markdown alongside the code change.
3. In CI, regenerate into a scratch directory and fail the build if the
   output differs from what is committed. A diff means the docs were not
   regenerated after a code change.

## Output shape

For a documented function, the rendered section looks like this:

````markdown
## `fn evaluate_flag`

```rust
pub async fn evaluate_flag( state: Arc<FlagState>, key: &str, rollout: u8, ) -> Result<bool, Error>
```

Evaluate a flag for one subject.
Returns false when the flag is unknown.

**Parameters**

- `state`: `Arc<FlagState>`
- `key`: `&str`
- `rollout`: `u8`

**Returns**: `Result<bool, Error>`

**Example** (from `test_evaluate_flag_returns_true`)

```rust
let state = Arc::new(FlagState::default());
assert!(evaluate_flag(state, "beta", 100).await.unwrap());
```
````

## Limitations

- Impl blocks are scanned like any other source region, so inherent methods
  appear as top-level items rather than nested under their type.
- Trait implementations of `Default`, `Serialize`, and similar are not
  reported, since only `pub` item declarations are collected.
- Example matching is name-based, so a test whose name does not contain the
  item name is not attached to it.
