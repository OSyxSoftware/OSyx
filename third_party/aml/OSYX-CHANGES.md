# aml 0.16.4 in OSyx

Source: crates.io `aml` 0.16.4, <https://github.com/rust-osdev/acpi>,
licensed MIT OR Apache-2.0 (copyright Isaac Woods and the rust-osdev
contributors). The license texts are those of the upstream repository.

Changes for OSyx (`src/lib.rs`):

- `Handler` has `read_ec_u8` / `write_ec_u8` (default: no embedded
  controller), and `EmbeddedControl` operation regions are read and written
  byte by byte through them. Laptop batteries (`_BST`, `_BIF`) and AC
  adapters (`_PSR`) usually read their fields from the EC.
- Region spaces the crate does not support return
  `AmlError::FieldInvalidAccessSize` instead of `unimplemented!()`.
