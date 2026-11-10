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
- Operators firmware battery and EC methods use but 0.16.4 lacked:
  `Subtract`, `Multiply`, `Divide`, `Mod`, `Or`, `XOr`, `NAnd`, `NOr`, `Not`,
  `LNot`, `SizeOf`, `Index` and `DerefOf` (`Index` returns the element by
  value, so `DerefOf` passes it through).
- `Index` as a store target (`Store(x, Index(PKG, 2))`, the usual way a
  firmware method fills the `_BST` package), for named objects and locals
  holding packages (`Target::Index`).
- Package elements may be locals and arguments of the running method
  (`Package () { Local0, Local1 }`), taken by value.
- An `If` without `Else` at the very end of a block (`If (a) { If (b) {
  Return (x) } }`, the shape of every `_DSM`) no longer fails with
  `UnexpectedEndOfStream`: the end of the stream means "no `Else`".

## Running real laptop firmware

Found by running tables compiled with the reference compiler (`iasl`) and
written the way laptop firmware is (`userspace/battery/tests/fixtures`).
Before these changes a ThinkPad-style table lost every method of its battery
device and OSyx showed no battery state.

Loading tables:

- `Package (N) { ... }` with fewer initializers than `N` (`Package (0x04) {}`,
  the usual declaration of a package filled later) is valid: the missing
  elements are zero. It used to be `MalformedPackage`, and the error dropped
  the rest of the table. `VarPackage` pads the same way.
- An object that cannot be parsed while a table loads costs that object, or
  at worst the rest of its scope, not the rest of the table: `term_list`
  skips it by its `PkgLength` and goes on. What was skipped is kept for the
  host (`AmlContext::take_load_errors`).
- `initialize_objects` goes on after an `_STA` or `_INI` that failed, and
  returns the first error once every device had its turn.
- `BankField` is `region, bank register, bank value` (it was parsed as
  `region, value, register`) and its fields work (`AmlValue::BankField`).
  `IndexField` fields work (`AmlValue::IndexField`).
- `DataTableRegion` defines a region (nothing can read it) so the fields on
  it are named; region spaces above 9 are kept as `OemDefined`.
- A `Connection (Buffer)` field element no longer fails the field list.

Running methods:

- `Acquire (mutex, timeout)`: the timeout is a WordData, not a TermArg. Read
  as a TermArg, `0xFFFF` left a stray `0xFF` and every method that takes a
  mutex failed with `UnexpectedByte(255)`.
- A name of a field used as an operand is the field's value, read then. It
  used to be the field object, read whenever someone later asked for an
  integer: after the firmware had selected another embedded controller page.
  `Store` stores values, never field objects, and copies buffers.
- Fields wider than 64 bits are buffers, read and written a byte at a time
  (the model and maker strings a firmware keeps in its EC). Fields are
  accessed in units of their access width, as many as they touch: a 16-bit
  field at an odd byte of a `ByteAcc` region is two byte accesses.
- A package passed to a method is the caller's package: `Arg1 [2] = x`
  reaches the named object (immediately) or the caller's local or argument
  (when the callee returns), unless the callee stored a new object in the
  argument first (`ArgSource`).
- `Index` targets: elements of buffers and strings, of arguments, and of
  elements (`DerefOf (PKG [1]) [2] = x`). `DerefOf (Index (...))` is a
  SuperName for `ObjectType`, `SizeOf`, `RefOf`, `CondRefOf`.
- A store to a named integer, string or buffer converts to the target's
  type; a named buffer keeps its storage (buffer fields created on it stay
  attached) and its length.
- `CondRefOf (x, target)` stores the object in the target (it was
  `Unimplemented`; `If (CondRefOf (\_OSI, Local0))` is in the `_INI` of nearly
  every firmware). Its first operand is a SuperName.
- New: `RefOf`, `FindSetLeftBit`, `FindSetRightBit`, `Match`, `Timer`
  (`Handler::timer_100ns`), `FromBCD`, `ToBCD`, `Load`, `LoadTable`, `Unload`
  (no table is loaded; the statements are consumed).
- Shifts by 64 or more give zero instead of an error.
- Objects a method creates (`Name (BUF, ...)` in a method body) are freed
  when it returns; a method may call itself.

No panics on firmware input:

- Buffer fields of 33 to 64 bits (`CreateQWordField`) panicked in `bitvec`;
  buffer fields are now read and written by plain bit loops.
- `Fatal` logs by default instead of panicking.
- A stale object handle, and a `PCI_Config` region without a device, are
  errors instead of `unwrap` panics.

`Handler::write_u8/16/32/64` take `&self` (like the I/O port writes): a
field's index register is written while the field is being read.
