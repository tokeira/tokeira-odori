# odori

`odori` is the facade crate for a minimal Rust agent framework where every run
is durably executed. It re-exports the framework primitives, providers,
embedded-engine integration, and the preview MCP bridge behind one dependency.

The facade exports `Engine` and the engine's `EmbeddedEngineConfig` shapes.
Applications may select ephemeral or snapshotted in-memory storage, managed
Aurora DSQL, or an existing canonical DSQL endpoint without dropping to an
engine-specific crate. Typed startup failures, the cluster/schema startup
report, and measured startup time remain visible at this boundary.

Start with the [Odori quickstart](https://github.com/tokeira/tokeira-odori#quickstart)
or read the [framework guides](https://github.com/tokeira/tokeira-odori/tree/main/docs).

Licensed under Apache-2.0.
