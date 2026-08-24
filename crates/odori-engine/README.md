# odori-engine

`odori-engine` connects Odori's durable run-loop worker to an embedded
[tokeira](https://github.com/tokeira/tokeira) engine. The connection is
in-process: no TCP, no ports, no daemon.

Its `Engine` wrapper forwards the embedded engine's storage configuration:
process-local in-memory storage with an optional snapshot policy, managed
Aurora DSQL create-or-recover, or an existing canonical DSQL endpoint. Startup
returns the engine's typed error unchanged. A successful engine exposes the
engine startup report and Odori's measured startup duration before its service
override is connected to `OdoriRuntime`.

Most applications should depend on the [`odori`](https://crates.io/crates/odori)
facade crate. See the [durability guide](https://github.com/tokeira/tokeira-odori#durability)
for the crash, resume, rewind, and fork model.

Licensed under Apache-2.0.
