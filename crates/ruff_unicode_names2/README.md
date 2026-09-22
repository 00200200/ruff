This is a vendored copy of [`unicode_names2`] from [PR #63], which adds an
alias-lookup API. That API lets Ruff preserve Python's strict Unicode-name matching while
using Unicode 17 character data.

The runtime and generator implementations are unchanged from that PR. Cargo
metadata is adapted to the Ruff workspace, and the nightly-only test module is
disabled during Clippy checks. The generator is vendored as
`ruff_unicode_names2_generator`, and `build.rs` generates the compressed name
tables and single alias table during compilation, as upstream does.

Ruff's strict matching is implemented in `ruff_python_parser`. Once an upstream
release includes the alias-lookup API, the vendored crates can be replaced with
the published dependency.

[pr #63]: https://github.com/progval/unicode_names2/pull/63
[`unicode_names2`]: https://github.com/progval/unicode_names2
