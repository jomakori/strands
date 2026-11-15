//! Library surface of the strands server binary. Split out so integration
//! tests (e.g. `tests/schema_shape.rs`) can reach the migration machinery
//! without binding a socket or going through `main()`.

pub mod db;
