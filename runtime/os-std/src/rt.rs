//! Program start-up and exit status conventions.
//!
//! On OSyx the runtime provides `_start`: it records `argc`/`argv` from the
//! System V initial stack, calls the function registered with [`entry!`] and
//! exits with the status that function's return value reports. A panic prints
//! the message to standard error and exits with status 101 (as in std).

use core::fmt::Debug;

/// Values a program's `main` may return, converted to an exit status.
pub trait Termination {
    fn report(self) -> i32;
}

impl Termination for () {
    fn report(self) -> i32 {
        0
    }
}

impl Termination for i32 {
    fn report(self) -> i32 {
        self
    }
}

impl<T: Termination, E: Debug> Termination for Result<T, E> {
    fn report(self) -> i32 {
        match self {
            Ok(v) => v.report(),
            Err(e) => {
                crate::eprintln!("Error: {:?}", e);
                1
            }
        }
    }
}

/// Registers `main` as the program entry point:
/// `os_std::entry!(main);` with `fn main()`, `fn main() -> i32` or
/// `fn main() -> Result<(), E>`.
#[macro_export]
macro_rules! entry {
    ($main:path) => {
        #[no_mangle]
        pub fn __osyx_user_main() -> i32 {
            $crate::rt::Termination::report($main())
        }
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn termination_codes() {
        assert_eq!(().report(), 0);
        assert_eq!(7.report(), 7);
        assert_eq!(Ok::<(), &str>(()).report(), 0);
        assert_eq!(Ok::<i32, &str>(3).report(), 3);
        assert_eq!(Err::<(), &str>("boom").report(), 1);
    }
}
