use crate::{misc::ArgNum, AmlContext, AmlError, AmlHandle, AmlName};
use alloc::{
    string::{String, ToString},
    sync::Arc,
    vec::Vec,
};
use bit_field::BitField;
use core::{cmp, fmt, fmt::Debug};
use spinning_top::Spinlock;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum RegionSpace {
    SystemMemory,
    SystemIo,
    PciConfig,
    EmbeddedControl,
    SMBus,
    SystemCmos,
    PciBarTarget,
    IPMI,
    GeneralPurposeIo,
    GenericSerialBus,
    OemDefined(u8),
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldAccessType {
    Any,
    Byte,
    Word,
    DWord,
    QWord,
    Buffer,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum FieldUpdateRule {
    Preserve,
    WriteAsOnes,
    WriteAsZeros,
}

// TODO: custom debug impl
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct FieldFlags(u8);

impl FieldFlags {
    pub fn new(value: u8) -> FieldFlags {
        FieldFlags(value)
    }

    pub fn access_type(&self) -> Result<FieldAccessType, AmlError> {
        match self.0.get_bits(0..4) {
            0 => Ok(FieldAccessType::Any),
            1 => Ok(FieldAccessType::Byte),
            2 => Ok(FieldAccessType::Word),
            3 => Ok(FieldAccessType::DWord),
            4 => Ok(FieldAccessType::QWord),
            5 => Ok(FieldAccessType::Buffer),
            _ => Err(AmlError::InvalidFieldFlags),
        }
    }

    pub fn lock_rule(&self) -> bool {
        self.0.get_bit(4)
    }

    pub fn field_update_rule(&self) -> Result<FieldUpdateRule, AmlError> {
        match self.0.get_bits(5..7) {
            0 => Ok(FieldUpdateRule::Preserve),
            1 => Ok(FieldUpdateRule::WriteAsOnes),
            2 => Ok(FieldUpdateRule::WriteAsZeros),
            _ => Err(AmlError::InvalidFieldFlags),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MethodFlags(u8);

impl MethodFlags {
    pub fn new(arg_count: u8, serialize: bool, sync_level: u8) -> MethodFlags {
        assert!(arg_count <= 7);
        assert!(sync_level <= 15);

        let mut value = 0;
        value.set_bits(0..3, arg_count);
        value.set_bit(3, serialize);
        value.set_bits(4..8, sync_level);
        MethodFlags(value)
    }

    pub fn from(value: u8) -> MethodFlags {
        MethodFlags(value)
    }

    pub fn arg_count(&self) -> u8 {
        self.0.get_bits(0..3)
    }

    pub fn serialize(&self) -> bool {
        self.0.get_bit(3)
    }

    pub fn sync_level(&self) -> u8 {
        self.0.get_bits(4..8)
    }
}

/// Representation of the return value of a `_STA` method, which represents the status of an object. It must be
/// evaluated, if present, before evaluating the `_INI` method for an device.
///
/// The `Default` implementation of this type is the correct value to use if a device doesn't have a `_STA` object
/// to evaluate.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct StatusObject {
    /// Whether the device is physically present. If this is `false`, `enabled` should also be `false` (i.e. a
    /// device that is not present can't be enabled). However, this is not enforced here if the firmware is doing
    /// something wrong.
    pub present: bool,
    /// Whether the device is enabled. Both `present` and `enabled` must be `true` for the device to decode its
    /// hardware resources.
    pub enabled: bool,
    pub show_in_ui: bool,
    pub functional: bool,
    /// Only applicable for Control Method Battery Devices (`PNP0C0A`). For all other devices, ignore this value.
    pub battery_present: bool,
}

impl Default for StatusObject {
    fn default() -> Self {
        StatusObject { present: true, enabled: true, show_in_ui: true, functional: true, battery_present: true }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum AmlType {
    Uninitialized,
    Buffer,
    BufferField,
    /// Handle to a definition block handle. Returned by the `Load` operator.
    DdbHandle,
    DebugObject,
    Event,
    FieldUnit,
    Device,
    Integer,
    Method,
    Mutex,
    ObjReference,
    OpRegion,
    Package,
    PowerResource,
    Processor,
    RawDataBuffer,
    String,
    ThermalZone,
}

#[derive(Clone)]
pub enum MethodCode {
    Aml(Vec<u8>),
    Native(Arc<dyn Fn(&mut AmlContext) -> Result<AmlValue, AmlError> + Send + Sync>),
}

impl fmt::Debug for MethodCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MethodCode::Aml(ref code) => write!(f, "AML({:x?})", code),
            MethodCode::Native(_) => write!(f, "(native method)"),
        }
    }
}

#[derive(Clone, Debug)]
pub enum AmlValue {
    Boolean(bool),
    Integer(u64),
    String(String),
    /// Describes an operation region. Some regions require other objects to be declared under their parent device
    /// (e.g. an `_ADR` object for a `PciConfig` region), in which case an absolute path to the object is stored in
    /// `parent_device`.
    OpRegion {
        region: RegionSpace,
        offset: u64,
        length: u64,
        parent_device: Option<AmlName>,
    },
    /// Describes a field unit within an operation region.
    Field {
        region: AmlHandle,
        flags: FieldFlags,
        offset: u64,
        length: u64,
    },
    /// OSyx: a field of an `IndexField`: the byte at `offset / 8` is
    /// selected by writing its number to the `index` field and then read or
    /// written through the `data` field.
    IndexField {
        index: AmlHandle,
        data: AmlHandle,
        flags: FieldFlags,
        offset: u64,
        length: u64,
    },
    /// OSyx: a field of a `BankField`: a field of `region` that is only
    /// valid while the `bank` field holds `bank_value`.
    BankField {
        region: AmlHandle,
        bank: AmlHandle,
        bank_value: u64,
        flags: FieldFlags,
        offset: u64,
        length: u64,
    },
    Device,
    Method {
        flags: MethodFlags,
        code: MethodCode,
    },
    Buffer(Arc<Spinlock<Vec<u8>>>),
    BufferField {
        buffer_data: Arc<Spinlock<Vec<u8>>>,
        /// In bits.
        offset: u64,
        /// In bits.
        length: u64,
    },
    Processor {
        id: u8,
        pblk_address: u32,
        pblk_len: u8,
    },
    Mutex {
        sync_level: u8,
    },
    /// OSyx: an event object (`DefEvent`). Without threads inside the
    /// interpreter nothing can signal or wait on it, so it only exists so
    /// `Signal`, `Reset` and `Wait` find their object.
    Event,
    // TODO: I think this will need to be `Arc`ed as well, as `Index` can be used on both Buffers and Packages
    Package(Vec<AmlValue>),
    PowerResource {
        system_level: u8,
        resource_order: u16,
    },
    ThermalZone,
}

impl AmlValue {
    pub fn zero() -> AmlValue {
        AmlValue::Integer(0)
    }

    pub fn one() -> AmlValue {
        AmlValue::Integer(1)
    }

    pub fn ones() -> AmlValue {
        AmlValue::Integer(u64::max_value())
    }

    pub fn native_method<F>(arg_count: u8, serialize: bool, sync_level: u8, f: F) -> AmlValue
    where
        F: (Fn(&mut AmlContext) -> Result<AmlValue, AmlError>) + 'static + Send + Sync,
    {
        let flags = MethodFlags::new(arg_count, serialize, sync_level);
        AmlValue::Method { flags, code: MethodCode::Native(Arc::new(f)) }
    }

    pub fn type_of(&self) -> AmlType {
        match self {
            AmlValue::Boolean(_) => AmlType::Integer,
            AmlValue::Integer(_) => AmlType::Integer,
            AmlValue::String(_) => AmlType::String,
            AmlValue::OpRegion { .. } => AmlType::OpRegion,
            AmlValue::Field { .. } | AmlValue::IndexField { .. } | AmlValue::BankField { .. } => AmlType::FieldUnit,
            AmlValue::Device => AmlType::Device,
            AmlValue::Method { .. } => AmlType::Method,
            AmlValue::Buffer(_) => AmlType::Buffer,
            AmlValue::BufferField { .. } => AmlType::BufferField,
            AmlValue::Processor { .. } => AmlType::Processor,
            AmlValue::Mutex { .. } => AmlType::Mutex,
            AmlValue::Event => AmlType::Event,
            AmlValue::Package(_) => AmlType::Package,
            AmlValue::PowerResource { .. } => AmlType::PowerResource,
            AmlValue::ThermalZone => AmlType::ThermalZone,
        }
    }

    pub fn as_bool(&self) -> Result<bool, AmlError> {
        match self {
            AmlValue::Boolean(value) => Ok(*value),
            AmlValue::Integer(value) => Ok(*value != 0),
            _ => Err(AmlError::IncompatibleValueConversion { current: self.type_of(), target: AmlType::Integer }),
        }
    }

    pub fn as_integer(&self, context: &AmlContext) -> Result<u64, AmlError> {
        match self {
            AmlValue::Integer(value) => Ok(*value),
            AmlValue::Boolean(value) => Ok(if *value { u64::max_value() } else { 0 }),
            AmlValue::Buffer(ref bytes) => {
                /*
                 * "The first 8 bytes of the buffer are converted to an integer, taking the first
                 * byte as the least significant byte of the integer. A zero-length buffer is
                 * illegal." - §19.6.140
                 *
                 * XXX: Buffers with length `0` appear in real tables, so we return `0` for them.
                 */
                let bytes = bytes.lock();
                let bytes = if bytes.len() > 8 { &bytes[0..8] } else { &bytes[..] };

                Ok(bytes.iter().rev().fold(0u64, |mut i, &popped| {
                    i <<= 8;
                    i += popped as u64;
                    i
                }))
            }
            /*
             * Read from a field or buffer field. These can return either a `Buffer` or an `Integer`, so we make sure to call
             * `as_integer` on the result.
             */
            AmlValue::Field { .. } | AmlValue::IndexField { .. } | AmlValue::BankField { .. } => self.read_field(context)?.as_integer(context),
            AmlValue::BufferField { .. } => self.read_buffer_field(context)?.as_integer(context),
            /*
             * OSyx: "0x" marks hexadecimal, otherwise the leading decimal
             * digits count (the implicit conversion of §19.3.5.7).
             */
            AmlValue::String(s) => {
                let s = s.trim();
                Ok(match s.strip_prefix("0x").or(s.strip_prefix("0X")) {
                    Some(hex) => {
                        let end = hex.find(|c: char| !c.is_ascii_hexdigit()).unwrap_or(hex.len());
                        u64::from_str_radix(&hex[..end], 16).unwrap_or(0)
                    }
                    None => {
                        let end = s.find(|c: char| !c.is_ascii_digit()).unwrap_or(s.len());
                        s[..end].parse().unwrap_or(0)
                    }
                })
            }

            _ => Err(AmlError::IncompatibleValueConversion { current: self.type_of(), target: AmlType::Integer }),
        }
    }

    pub fn as_buffer(&self, context: &AmlContext) -> Result<Arc<Spinlock<Vec<u8>>>, AmlError> {
        match self {
            AmlValue::Buffer(ref bytes) => Ok(bytes.clone()),
            // OSyx: an integer is its eight bytes, a string its characters
            // and the terminating zero.
            AmlValue::Integer(value) => Ok(Arc::new(Spinlock::new(value.to_le_bytes().to_vec()))),
            AmlValue::Boolean(value) => {
                Ok(Arc::new(Spinlock::new(if *value { u64::MAX } else { 0 }.to_le_bytes().to_vec())))
            }
            AmlValue::String(s) => {
                let mut bytes = s.as_bytes().to_vec();
                bytes.push(0);
                Ok(Arc::new(Spinlock::new(bytes)))
            }
            AmlValue::Field { .. } | AmlValue::IndexField { .. } | AmlValue::BankField { .. } => self.read_field(context)?.as_buffer(context),
            AmlValue::BufferField { .. } => self.read_buffer_field(context)?.as_buffer(context),
            _ => Err(AmlError::IncompatibleValueConversion { current: self.type_of(), target: AmlType::Buffer }),
        }
    }

    pub fn as_string(&self, context: &AmlContext) -> Result<String, AmlError> {
        match self {
            AmlValue::String(ref string) => Ok(string.clone()),
            // OSyx: a buffer is text up to its first zero; an integer its
            // hexadecimal digits.
            AmlValue::Buffer(bytes) => {
                let bytes = bytes.lock();
                let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
                Ok(bytes[..end].iter().map(|&b| b as char).collect())
            }
            AmlValue::Integer(value) => Ok(alloc::format!("{value:X}")),
            AmlValue::Field { .. } | AmlValue::IndexField { .. } | AmlValue::BankField { .. } => self.read_field(context)?.as_string(context),
            AmlValue::BufferField { .. } => self.read_buffer_field(context)?.as_string(context),
            _ => Err(AmlError::IncompatibleValueConversion { current: self.type_of(), target: AmlType::String }),
        }
    }

    /// OSyx: the value an operand evaluates to: fields are read (once, now),
    /// everything else is itself. A field stored as an object would be read
    /// again later, when the firmware has long selected another page of the
    /// embedded controller.
    pub fn resolved(self, context: &AmlContext) -> Result<AmlValue, AmlError> {
        match self {
            AmlValue::Field { .. } | AmlValue::IndexField { .. } | AmlValue::BankField { .. } => self.read_field(context),
            AmlValue::BufferField { .. } => self.read_buffer_field(context),
            other => Ok(other),
        }
    }

    /// OSyx: a copy that shares no buffer with `self` (`Store` copies).
    pub fn detached(&self) -> AmlValue {
        match self {
            AmlValue::Buffer(bytes) => AmlValue::Buffer(Arc::new(Spinlock::new(bytes.lock().clone()))),
            AmlValue::Package(items) => AmlValue::Package(items.iter().map(AmlValue::detached).collect()),
            other => other.clone(),
        }
    }

    /// Converts an `AmlValue` to the representation that should be used when concatenating it with other values,
    /// primarily by the `DefConcat` opcode. This will always produce a `AmlValue::Integer`, `AmlValue::String`, or
    /// `AmlValue::Buffer`, with other types being converted to strings containing the name of their type.
    pub fn as_concat_type(&self) -> AmlValue {
        match self.type_of() {
            AmlType::Integer => self.clone(),
            AmlType::String => self.clone(),
            AmlType::Buffer => self.clone(),

            AmlType::Uninitialized => AmlValue::String("[Uninitialized]".to_string()),
            AmlType::BufferField => AmlValue::String("[Buffer Field]".to_string()),
            AmlType::DdbHandle => AmlValue::String("[Ddb Handle]".to_string()),
            AmlType::DebugObject => AmlValue::String("[Debug Object]".to_string()),
            AmlType::Event => AmlValue::String("[Event]".to_string()),
            AmlType::FieldUnit => AmlValue::String("[Field]".to_string()),
            AmlType::Device => AmlValue::String("[Device]".to_string()),
            AmlType::Method => AmlValue::String("[Control Method]".to_string()),
            AmlType::Mutex => AmlValue::String("[Mutex]".to_string()),
            AmlType::ObjReference => AmlValue::String("[Obj Reference]".to_string()),
            AmlType::OpRegion => AmlValue::String("[Operation Region]".to_string()),
            AmlType::Package => AmlValue::String("[Package]".to_string()),
            AmlType::Processor => AmlValue::String("[Processor]".to_string()),
            AmlType::PowerResource => AmlValue::String("[Power Resource]".to_string()),
            AmlType::RawDataBuffer => AmlValue::String("[Raw Data Buffer]".to_string()),
            AmlType::ThermalZone => AmlValue::String("[Thermal Zone]".to_string()),
        }
    }

    /// Turns an `AmlValue` returned from a `_STA` method into a `StatusObject`. Should only be called for values
    /// returned from `_STA`. If you need a `StatusObject`, but the device does not have a `_STA` method, use
    /// `StatusObject::default()`.
    pub fn as_status(&self) -> Result<StatusObject, AmlError> {
        match self {
            AmlValue::Integer(value) => {
                /*
                 * Bits 5+ are reserved and are expected to be cleared.
                 */
                if value.get_bits(5..64) != 0 {
                    return Err(AmlError::InvalidStatusObject);
                }

                Ok(StatusObject {
                    present: value.get_bit(0),
                    enabled: value.get_bit(1),
                    show_in_ui: value.get_bit(2),
                    functional: value.get_bit(3),
                    battery_present: value.get_bit(4),
                })
            }

            _ => Err(AmlError::InvalidStatusObject),
        }
    }

    /// Convert this value to a value of the same data, but with the given AML type, if possible,
    /// by converting the implicit conversions described in §19.3.5 of the spec.
    ///
    /// The implicit conversions applied are:
    ///     `Buffer` from: `Integer`, `String`, `Debug`
    ///     `BufferField` from: `Integer`, `Buffer`, `String`, `Debug`
    ///     `DdbHandle` from: `Integer`, `Debug`
    ///     `FieldUnit` from: `Integer`,`Buffer`, `String`, `Debug`
    ///     `Integer` from: `Buffer`, `BufferField`, `DdbHandle`, `FieldUnit`, `String`, `Debug`
    ///     `Package` from: `Debug`
    ///     `String` from: `Integer`, `Buffer`, `Debug`
    pub fn as_type(&self, desired_type: AmlType, context: &AmlContext) -> Result<AmlValue, AmlError> {
        // If the value is already of the correct type, just return it as is
        if self.type_of() == desired_type {
            return Ok(self.clone());
        }

        // TODO: implement all of the rules
        match desired_type {
            AmlType::Integer => self.as_integer(context).map(|value| AmlValue::Integer(value)),
            AmlType::Buffer => self.as_buffer(context).map(|value| AmlValue::Buffer(value)),
            AmlType::String => self.as_string(context).map(AmlValue::String),
            AmlType::FieldUnit => Err(AmlError::IncompatibleValueConversion {
                current: self.type_of(),
                target: AmlType::FieldUnit,
            }),
            _ => Err(AmlError::IncompatibleValueConversion { current: self.type_of(), target: desired_type }),
        }
    }

    /// OSyx: the largest single access an operation region space allows.
    fn maximum_access_size(context: &AmlContext, region: AmlHandle) -> Result<u64, AmlError> {
        if let AmlValue::OpRegion { region, .. } = context.namespace.get(region)? {
            Ok(match region {
                RegionSpace::SystemMemory | RegionSpace::EmbeddedControl => 64,
                RegionSpace::SystemIo | RegionSpace::PciConfig => 32,
                _ => return Err(AmlError::FieldInvalidAccessSize),
            })
        } else {
            Err(AmlError::FieldRegionIsNotOpRegion)
        }
    }

    /// OSyx: the width in bits of one access to `region` for a field with
    /// `flags`: the field's access type, limited to what the region's
    /// address space can do.
    fn access_unit(context: &AmlContext, region: AmlHandle, flags: &FieldFlags) -> Result<u64, AmlError> {
        Ok(u64::min(Self::minimum_access_size(flags)?, Self::maximum_access_size(context, region)?))
    }

    fn low_bits(length: u64) -> u64 {
        if length >= 64 {
            u64::MAX
        } else {
            (1u64 << length) - 1
        }
    }

    fn minimum_access_size(flags: &FieldFlags) -> Result<u64, AmlError> {
        Ok(match flags.access_type()? {
            FieldAccessType::Any | FieldAccessType::Byte | FieldAccessType::Buffer => 8,
            FieldAccessType::Word => 16,
            FieldAccessType::DWord => 32,
            FieldAccessType::QWord => 64,
        })
    }

    /// OSyx: reads `length` (at most 64) bits at bit `offset` of a field's
    /// backing store: an operation region, or an index/data register pair.
    fn read_bits(&self, context: &AmlContext, offset: u64, length: u64) -> Result<u64, AmlError> {
        match self {
            AmlValue::Field { region, flags, .. } => {
                /*
                 * The region is accessed in units of the field's access
                 * width, as many as the field touches: a 16-bit field at
                 * an odd byte of a `ByteAcc` region is two byte reads,
                 * wherever it starts.
                 */
                let unit = Self::access_unit(context, *region, flags)?;
                let (first, last) = (offset / unit, (offset + length - 1) / unit);
                let mut raw = 0u128;
                for (i, n) in (first..=last).enumerate() {
                    raw |= (context.read_region(*region, n * unit / 8, unit)? as u128) << (unit as usize * i);
                }
                Ok(((raw >> (offset - first * unit)) as u64) & Self::low_bits(length))
            }
            AmlValue::BankField { region, bank, bank_value, flags, .. } => {
                context.namespace.get(*bank)?.clone().write_field_value(AmlValue::Integer(*bank_value), context)?;
                AmlValue::Field { region: *region, flags: *flags, offset, length }.read_bits(context, offset, length)
            }
            AmlValue::IndexField { index, data, .. } => {
                // Byte by byte: select, then read through the data register.
                let (first, last) = (offset / 8, (offset + length - 1) / 8);
                let mut raw = 0u128;
                for (i, byte) in (first..=last).enumerate() {
                    context.namespace.get(*index)?.clone().write_field_value(AmlValue::Integer(byte), context)?;
                    let value = context.namespace.get(*data)?.read_field(context)?.as_integer(context)?;
                    raw |= ((value & 0xff) as u128) << (8 * i);
                }
                Ok(((raw >> (offset - first * 8)) as u64) & Self::low_bits(length))
            }
            _ => Err(AmlError::IncompatibleValueConversion { current: self.type_of(), target: AmlType::FieldUnit }),
        }
    }

    /// OSyx: writes the low `length` (at most 64) bits of `value` at bit
    /// `offset` of a field's backing store.
    fn write_bits(&self, context: &AmlContext, offset: u64, length: u64, value: u64) -> Result<(), AmlError> {
        match self {
            AmlValue::Field { region, flags, .. } => {
                let unit = Self::access_unit(context, *region, flags)?;
                let (first, last) = (offset / unit, (offset + length - 1) / unit);
                let shift = offset - first * unit;
                let mask = (Self::low_bits(length) as u128) << shift;
                let bits = ((value as u128) << shift) & mask;
                let unit_mask = Self::low_bits(unit);
                for (i, n) in (first..=last).enumerate() {
                    let covered = (mask >> (unit as usize * i)) as u64 & unit_mask;
                    let new = (bits >> (unit as usize * i)) as u64 & unit_mask;
                    // A unit the field covers whole is written without a read.
                    let old = if covered == unit_mask {
                        0
                    } else {
                        match flags.field_update_rule()? {
                            FieldUpdateRule::Preserve => context.read_region(*region, n * unit / 8, unit)?,
                            FieldUpdateRule::WriteAsOnes => u64::MAX,
                            FieldUpdateRule::WriteAsZeros => 0,
                        }
                    };
                    context.write_region(*region, n * unit / 8, unit, (old & !covered) | new)?;
                }
                Ok(())
            }
            AmlValue::BankField { region, bank, bank_value, flags, .. } => {
                context.namespace.get(*bank)?.clone().write_field_value(AmlValue::Integer(*bank_value), context)?;
                AmlValue::Field { region: *region, flags: *flags, offset, length }.write_bits(context, offset, length, value)
            }
            AmlValue::IndexField { index, data, flags, .. } => {
                let (first, last) = (offset / 8, (offset + length - 1) / 8);
                let shift = offset - first * 8;
                let mask = (Self::low_bits(length) as u128) << shift;
                let bits = ((value as u128) << shift) & mask;
                for (i, byte) in (first..=last).enumerate() {
                    let byte_mask = (mask >> (8 * i)) as u8;
                    let new = (bits >> (8 * i)) as u8;
                    context.namespace.get(*index)?.clone().write_field_value(AmlValue::Integer(byte), context)?;
                    let old = if byte_mask == 0xff {
                        0
                    } else {
                        match flags.field_update_rule()? {
                            FieldUpdateRule::Preserve => {
                                context.namespace.get(*data)?.read_field(context)?.as_integer(context)? as u8
                            }
                            FieldUpdateRule::WriteAsOnes => 0xff,
                            FieldUpdateRule::WriteAsZeros => 0,
                        }
                    };
                    let merged = (old & !byte_mask) | (new & byte_mask);
                    context.namespace.get(*data)?.clone().write_field_value(AmlValue::Integer(merged as u64), context)?;
                }
                Ok(())
            }
            _ => Err(AmlError::IncompatibleValueConversion { current: self.type_of(), target: AmlType::FieldUnit }),
        }
    }

    fn field_span(&self) -> Result<(u64, u64), AmlError> {
        match self {
            AmlValue::Field { offset, length, .. }
            | AmlValue::IndexField { offset, length, .. }
            | AmlValue::BankField { offset, length, .. } => Ok((*offset, *length)),
            _ => Err(AmlError::IncompatibleValueConversion { current: self.type_of(), target: AmlType::FieldUnit }),
        }
    }

    /// Reads from a field of an opregion, returning either a `AmlValue::Integer` or an `AmlValue::Buffer`,
    /// depending on the size of the field.
    ///
    /// OSyx: a field wider than 64 bits is a `Buffer`, read a byte at a
    /// time (the strings a firmware keeps in its embedded controller).
    pub fn read_field(&self, context: &AmlContext) -> Result<AmlValue, AmlError> {
        let (offset, length) = self.field_span()?;
        if length == 0 {
            return Err(AmlError::FieldInvalidAccessSize);
        }
        if length <= 64 {
            return Ok(AmlValue::Integer(self.read_bits(context, offset, length)?));
        }
        let mut bytes = Vec::with_capacity(((length + 7) / 8) as usize);
        let mut done = 0;
        while done < length {
            let n = u64::min(8, length - done);
            bytes.push(self.read_bits(context, offset + done, n)? as u8);
            done += n;
        }
        Ok(AmlValue::Buffer(Arc::new(Spinlock::new(bytes))))
    }

    pub fn write_field(&mut self, value: AmlValue, context: &mut AmlContext) -> Result<(), AmlError> {
        self.write_field_value(value, context)
    }

    /// OSyx: [`AmlValue::write_field`] without the exclusive borrows; an
    /// index register is written while a field is being read.
    pub(crate) fn write_field_value(&self, value: AmlValue, context: &AmlContext) -> Result<(), AmlError> {
        let (offset, length) = self.field_span()?;
        if length == 0 {
            return Err(AmlError::FieldInvalidAccessSize);
        }
        if length <= 64 {
            return self.write_bits(context, offset, length, value.as_integer(context)?);
        }
        // Wide field: the source's bytes, zero-extended.
        let source = value.as_buffer(context)?;
        let source = source.lock();
        let mut done = 0;
        while done < length {
            let n = u64::min(8, length - done);
            let byte = source.get((done / 8) as usize).copied().unwrap_or(0);
            self.write_bits(context, offset + done, n, byte as u64)?;
            done += n;
        }
        Ok(())
    }

    /// OSyx: the bits `offset .. offset + length` of `bytes`, least
    /// significant first, as bytes (the last one zero-padded).
    fn extract_bits(bytes: &[u8], offset: usize, length: usize) -> Vec<u8> {
        let mut out = alloc::vec![0u8; length.div_ceil(8)];
        for i in 0..length {
            let bit = offset + i;
            if bytes[bit / 8] >> (bit % 8) & 1 != 0 {
                out[i / 8] |= 1 << (i % 8);
            }
        }
        out
    }

    /// OSyx: reads a buffer field: an `Integer` up to 64 bits, a `Buffer`
    /// beyond. (Upstream 0.16.4 panicked on fields of 33 to 64 bits.)
    pub fn read_buffer_field(&self, _context: &AmlContext) -> Result<AmlValue, AmlError> {
        if let AmlValue::BufferField { buffer_data, offset, length } = self {
            let offset = *offset as usize;
            let length = *length as usize;
            let inner_data = buffer_data.lock();

            if offset.checked_add(length).is_none_or(|end| end > inner_data.len() * 8) {
                return Err(AmlError::BufferFieldIndexesOutOfBounds);
            }

            let bits = Self::extract_bits(&inner_data, offset, length);
            if length > 64 {
                Ok(AmlValue::Buffer(Arc::new(Spinlock::new(bits))))
            } else {
                let mut value = [0u8; 8];
                value[..bits.len()].copy_from_slice(&bits);
                Ok(AmlValue::Integer(u64::from_le_bytes(value)))
            }
        } else {
            Err(AmlError::IncompatibleValueConversion { current: self.type_of(), target: AmlType::BufferField })
        }
    }

    /// OSyx: writes a buffer field. The whole field is overwritten: the
    /// source is truncated or zero-extended to the field's length.
    pub fn write_buffer_field(&mut self, value: AmlValue, context: &mut AmlContext) -> Result<(), AmlError> {
        if let AmlValue::BufferField { buffer_data, offset, length } = self {
            let offset = *offset as usize;
            let length = *length as usize;
            let source: Vec<u8> = match value {
                AmlValue::Integer(value) => value.to_le_bytes().to_vec(),
                AmlValue::Boolean(value) => alloc::vec![value as u8],
                // Copied out first: the source may be the field's own buffer.
                AmlValue::Buffer(bytes) => bytes.lock().clone(),
                AmlValue::String(text) => text.into_bytes(),
                other @ (AmlValue::Field { .. }
                | AmlValue::IndexField { .. }
                | AmlValue::BankField { .. }
                | AmlValue::BufferField { .. }) => other.resolved(context)?.as_buffer(context)?.lock().clone(),
                other => return Err(AmlError::TypeCannotBeWrittenToBufferField(other.type_of())),
            };
            let mut inner_data = buffer_data.lock();
            if offset.checked_add(length).is_none_or(|end| end > inner_data.len() * 8) {
                return Err(AmlError::BufferFieldIndexesOutOfBounds);
            }
            for i in 0..length {
                let bit = source.get(i / 8).is_some_and(|byte| byte >> (i % 8) & 1 != 0);
                let at = offset + i;
                if bit {
                    inner_data[at / 8] |= 1 << (at % 8);
                } else {
                    inner_data[at / 8] &= !(1 << (at % 8));
                }
            }
            Ok(())
        } else {
            Err(AmlError::IncompatibleValueConversion { current: self.type_of(), target: AmlType::BufferField })
        }
    }

    /// Logically compare two `AmlValue`s, according to the rules that govern opcodes like `DefLEqual`, `DefLLess`,
    /// etc. The type of `self` dictates the type that `other` will be converted to, and the method by which the
    /// values will be compared:
    ///    - `Integer`s are simply compared by numeric comparison
    ///    - `String`s and `Buffer`s are compared lexicographically - `other` is compared byte-wise until a byte
    ///      is discovered that is either less or greater than the corresponding byte of `self`. If the bytes are
    ///      identical, the lengths are compared. Luckily, the Rust standard library implements lexicographic
    ///      comparison of strings and `[u8]` for us already.
    pub fn cmp(&self, other: AmlValue, context: &mut AmlContext) -> Result<cmp::Ordering, AmlError> {
        let self_inner = self.clone().resolved(context)?;

        match self_inner.type_of() {
            AmlType::Integer => Ok(self_inner.as_integer(context)?.cmp(&other.as_integer(context)?)),
            AmlType::Buffer => {
                // OSyx: both sides are copied out first: they may be the
                // same buffer, and the lock is not re-entrant.
                let left = self_inner.as_buffer(context)?.lock().clone();
                let right = other.as_buffer(context)?.lock().clone();
                Ok(left.cmp(&right))
            }
            AmlType::String => Ok(self_inner.as_string(context)?.cmp(&other.as_string(context)?)),
            typ => Err(AmlError::TypeCannotBeCompared(typ)),
        }
    }
}

/// A control method can take up to 7 arguments, each of which is an `AmlValue`.
#[derive(Clone, Default, Debug)]
pub struct Args(pub [Option<AmlValue>; 7]);

impl Args {
    pub const EMPTY: Self = Self([None, None, None, None, None, None, None]);

    pub fn from_list(list: Vec<AmlValue>) -> Result<Args, AmlError> {
        use core::convert::TryInto;

        if list.len() > 7 {
            return Err(AmlError::TooManyArgs);
        }

        let mut args: Vec<Option<AmlValue>> = list.into_iter().map(Option::Some).collect();
        args.extend(core::iter::repeat(None).take(7 - args.len()));
        Ok(Args(args.try_into().unwrap()))
    }

    pub fn arg(&self, arg: ArgNum) -> Result<&AmlValue, AmlError> {
        if arg > 6 {
            return Err(AmlError::InvalidArgAccess(arg));
        }

        self.0[arg as usize].as_ref().ok_or(AmlError::InvalidArgAccess(arg))
    }

    pub fn store_arg(&mut self, arg: ArgNum, value: AmlValue) -> Result<(), AmlError> {
        if arg > 6 {
            return Err(AmlError::InvalidArgAccess(arg));
        }

        self.0[arg as usize] = Some(value);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_utils::*;
    use core::cmp::Ordering;

    #[test]
    fn test_object_cmp() {
        let mut context = make_test_context();

        assert_eq!(AmlValue::Integer(76).cmp(AmlValue::Integer(89), &mut context), Ok(Ordering::Less));
        assert_eq!(AmlValue::Integer(11).cmp(AmlValue::Integer(11), &mut context), Ok(Ordering::Equal));
        assert_eq!(AmlValue::Integer(8362836690).cmp(AmlValue::Integer(1), &mut context), Ok(Ordering::Greater));

        // TODO: test the other combinations too, as well as conversions to the correct types for the second operand
    }
}
