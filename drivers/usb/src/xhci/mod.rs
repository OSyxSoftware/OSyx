//! xHCI data structures and register layouts (xHCI 1.2).

pub mod context;
pub mod regs;
pub mod ring;
pub mod trb;

/// Device Context Index of an endpoint: 1 for the default control endpoint,
/// `2 * number + direction` (IN = 1) otherwise (xHCI 4.5.1).
pub fn dci(endpoint_address: u8) -> u8 {
    let number = endpoint_address & 0x0F;
    if number == 0 {
        return 1;
    }
    number * 2 + u8::from(endpoint_address & 0x80 != 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_context_indices() {
        assert_eq!(dci(0x00), 1);
        assert_eq!(dci(0x80), 1);
        assert_eq!(dci(0x01), 2);
        assert_eq!(dci(0x81), 3);
        assert_eq!(dci(0x82), 5);
        assert_eq!(dci(0x0F), 30);
        assert_eq!(dci(0x8F), 31);
    }
}
