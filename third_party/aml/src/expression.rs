use crate::{
    misc::debug_obj,
    name_object::{name_string, simple_name, super_name, target},
    opcode::{self, ext_opcode, opcode},
    parser::{
        choice, comment_scope, n_of, take, take_to_end_of_pkglength, take_u16, try_with_context, ParseResult, Parser,
        Propagate,
    },
    pkg_length::pkg_length,
    term_object::{data_ref_object, term_arg, def_cond_ref_of},
    value::{AmlType, AmlValue, Args},
    AmlContext,
    AmlError,
    DebugVerbosity,
};
use alloc::{
    format,
    string::{String, ToString},
    sync::Arc,
    vec,
    vec::Vec,
};
use core::{cmp::Ordering, convert::TryInto, mem, ops::Deref};

pub fn expression_opcode<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * ExpressionOpcode := DefAquire | DefAdd | DefAnd | DefBuffer | DefConcat | DefConcatRes |
     *                     DefCondRefOf | DefCopyObject | DefDecrement | DefDerefOf | DefDivide |
     *                     DefFindSetLeftBit | DefFindSetRightBit | DefFromBCD | DefIncrement | DefIndex |
     *                     DefLAnd | DefLEqual | DefLGreater | DefLGreaterEqual | DefLLess | DefLLessEqual |
     *                     DefMid | DefLNot | DefLNotEqual | DefLoad | DefLoadTable | DefLOr | DefMatch | DefMod |
     *                     DefMultiply | DefNAnd | DefNOr | DefNot | DefObjectType | DefOr | DefPackage |
     *                     DefVarPackage | DefRefOf | DefShiftLeft | DefShiftRight | DefSizeOf | DefStore |
     *                     DefSubtract | DefTimer | DefToBCD | DefToBuffer | DefToDecimalString |
     *                     DefToHexString | DefToInteger | DefToString | DefWait | DefXOr | MethodInvocation
     */
    comment_scope(
        DebugVerbosity::AllScopes,
        "ExpressionOpcode",
        choice!(
            def_add(),
            def_subtract(),
            def_multiply(),
            def_divide(),
            def_mod(),
            def_or(),
            def_xor(),
            def_nand(),
            def_nor(),
            def_not(),
            def_index(),
            def_deref_of(),
            def_size_of(),
            def_and(),
            def_buffer(),
            def_concat(),
            def_concat_res(),
            def_increment(),
            def_decrement(),
            def_l_equal(),
            def_l_greater(),
            def_l_greater_equal(),
            def_l_less(),
            def_l_less_equal(),
            def_l_not_equal(),
            def_l_not(),
            def_l_and(),
            def_l_or(),
            def_mid(),
            def_object_type(),
            def_package(),
            def_shift_left(),
            def_shift_right(),
            def_store(),
            def_to_integer(),
            def_to_buffer(),
            def_to_string(),
            def_to_decimal_string(),
            def_to_hex_string(),
            def_copy_object(),
            def_acquire(),
            def_wait(),
            def_cond_ref_of(),
            def_ref_of(),
            def_find_set_left_bit(),
            def_find_set_right_bit(),
            def_match(),
            def_timer(),
            def_from_bcd(),
            def_to_bcd(),
            def_load_table(),
            method_invocation() // XXX: this must always appear last. See how we have to parse it to see why.
        ),
    )
}

pub fn def_add<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefAdd := 0x72 Operand Operand Target
     * Operand := TermArg => Integer
     */
    opcode(opcode::DEF_ADD_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefAdd",
            term_arg().then(term_arg()).then(target()).map_with_context(
                |((left_arg, right_arg), target), context| {
                    let left = try_with_context!(context, left_arg.as_integer(context));
                    let right = try_with_context!(context, right_arg.as_integer(context));
                    let result = AmlValue::Integer(left.wrapping_add(right));

                    try_with_context!(context, context.store(target, result.clone()));
                    (Ok(result), context)
                },
            ),
        ))
        .map(|((), result)| Ok(result))
}

pub fn def_and<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefAnd := 0x7b Operand Operand Target
     * Operand := TermArg => Integer
     */
    opcode(opcode::DEF_AND_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefAnd",
            term_arg().then(term_arg()).then(target()).map_with_context(
                |((left_arg, right_arg), target), context| {
                    let left = try_with_context!(context, left_arg.as_integer(context));
                    let right = try_with_context!(context, right_arg.as_integer(context));
                    let result = AmlValue::Integer(left & right);

                    try_with_context!(context, context.store(target, result.clone()));
                    (Ok(result), context)
                },
            ),
        ))
        .map(|((), result)| Ok(result))
}

pub fn def_buffer<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefBuffer := 0x11 PkgLength BufferSize ByteList
     * BufferSize := TermArg => Integer
     *
     * XXX: The spec says that zero-length buffers (e.g. the PkgLength is 0) are illegal, but
     * we've encountered them in QEMU-generated tables, so we return an empty buffer in these
     * cases.
     *
     * Uninitialized elements are initialized to zero.
     */
    opcode(opcode::DEF_BUFFER_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefBuffer",
            pkg_length().then(term_arg()).feed(|(pkg_length, buffer_size)| {
                take_to_end_of_pkglength(pkg_length).map_with_context(move |bytes, context| {
                    let buffer_size = try_with_context!(context, buffer_size.as_integer(context)) as usize;

                    if buffer_size < bytes.len() {
                        return (Err(Propagate::Err(AmlError::MalformedBuffer)), context);
                    }

                    let mut buffer = vec![0; buffer_size];
                    (&mut buffer[0..bytes.len()]).copy_from_slice(bytes);
                    (Ok(buffer), context)
                })
            }),
        ))
        .map(|((), buffer)| Ok(AmlValue::Buffer(Arc::new(spinning_top::Spinlock::new(buffer)))))
}

pub fn def_concat<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefConcat := 0x73 Data Data Target
     * Data := TermArg => ComputationalData
     */
    opcode(opcode::DEF_CONCAT_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefConcat",
            term_arg().then(term_arg()).then(target()).map_with_context(|((left, right), target), context| {
                let result = match left.as_concat_type() {
                    AmlValue::Integer(left) => {
                        let right = try_with_context!(context, right.as_integer(context));

                        let mut buffer = Vec::with_capacity(mem::size_of::<u64>() * 2);
                        buffer.extend_from_slice(&left.to_le_bytes());
                        buffer.extend_from_slice(&right.to_le_bytes());

                        AmlValue::Buffer(Arc::new(spinning_top::Spinlock::new(buffer)))
                    }
                    AmlValue::Buffer(left) => {
                        let mut new: Vec<u8> = left.lock().deref().clone();
                        new.extend(try_with_context!(context, right.as_buffer(context)).lock().iter());
                        AmlValue::Buffer(Arc::new(spinning_top::Spinlock::new(new)))
                    }
                    AmlValue::String(left) => {
                        let right = match right.as_concat_type() {
                            AmlValue::String(right) => right,
                            AmlValue::Integer(_) => try_with_context!(context, right.as_string(context)),
                            AmlValue::Buffer(_) => try_with_context!(context, right.as_string(context)),
                            _ => panic!("Invalid type returned from `as_concat_type`"),
                        };
                        AmlValue::String(left + &right)
                    }
                    _ => panic!("Invalid type returned from `as_concat_type`"),
                };

                try_with_context!(context, context.store(target, result.clone()));
                (Ok(result), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

pub fn def_concat_res<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefConcatRes := 0x84 BufData BufData Target
     * BufData := TermArg => Buffer
     */
    opcode(opcode::DEF_CONCAT_RES_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefConcatRes",
            term_arg().then(term_arg()).then(target()).map_with_context(|((left, right), target), context| {
                let left = try_with_context!(context, left.as_buffer(context));
                let right = try_with_context!(context, right.as_buffer(context));

                let left_len = left.lock().len();
                let right_len = right.lock().len();

                if left_len == 1 || right_len == 1 {
                    return (Err(Propagate::Err(AmlError::ResourceDescriptorTooShort)), context);
                }

                /*
                 * `left` and `right` are buffers of resource descriptors, which we're trying to concatenate into a
                 * new, single buffer containing all of the descriptors from the source buffers. We need to strip
                 * off the end tags (2 bytes from each buffer), and then add our own end tag.
                 *
                 * XXX: either buffer may be empty (contains no tags), and so our arithmetic has to be careful.
                 */
                let result = {
                    let mut result =
                        Vec::with_capacity(left_len.saturating_sub(2) + right_len.saturating_sub(2) + 2);
                    let left_contents = left.lock();
                    let right_contents = right.lock();
                    result.extend_from_slice(if left_len == 0 { &[] } else { &left_contents[..(left_len - 2)] });
                    result.extend_from_slice(if right_len == 0 {
                        &[]
                    } else {
                        &right_contents[..(right_len - 2)]
                    });

                    /*
                     * Construct a new end tag, including a new checksum:
                     *    | Bits        | Field             | Value                     |
                     *    |-------------|-------------------|---------------------------|
                     *    | 0-2         | Length - n bytes  | 1 (for checksum)          |
                     *    | 3-6         | Small item type   | 0x0f = end tag descriptor |
                     *    | 7           | 0 = small item    | 0                         |
                     */
                    result.push(0b01111001);
                    result.push(
                        result.iter().fold(0u8, |checksum, byte| checksum.wrapping_add(*byte)).wrapping_neg(),
                    );

                    AmlValue::Buffer(Arc::new(spinning_top::Spinlock::new(result)))
                };

                try_with_context!(context, context.store(target, result.clone()));
                (Ok(result), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

fn def_increment<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefIncrement := 0x75 SuperName
     */
    opcode(opcode::DEF_INCREMENT_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefIncrement",
            super_name().map_with_context(|addend, context| {
                let value = try_with_context!(context, context.read_target(&addend));
                let value = try_with_context!(context, value.as_integer(context));
                let new_value = AmlValue::Integer(value + 1);
                try_with_context!(context, context.store(addend, new_value.clone()));
                (Ok(new_value), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

fn def_decrement<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefDecrement := 0x76 SuperName
     */
    opcode(opcode::DEF_DECREMENT_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefDecrement",
            super_name().map_with_context(|minuend, context| {
                let value = try_with_context!(context, context.read_target(&minuend));
                let value = try_with_context!(context, value.as_integer(context));
                let new_value = AmlValue::Integer(value - 1);
                try_with_context!(context, context.store(minuend, new_value.clone()));
                (Ok(new_value), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

fn def_l_and<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefLAnd := 0x90 Operand Operand
     * Operand := TermArg => Integer
     */
    opcode(opcode::DEF_L_AND_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefLOr",
            term_arg().then(term_arg()).map_with_context(|(left_arg, right_arg), context| {
                let left = try_with_context!(context, left_arg.as_bool());
                let right = try_with_context!(context, right_arg.as_bool());
                (Ok(AmlValue::Boolean(left && right)), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

fn def_l_or<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefLOr := 0x91 Operand Operand
     * Operand := TermArg => Integer
     */
    opcode(opcode::DEF_L_OR_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefLOr",
            term_arg().then(term_arg()).map_with_context(|(left_arg, right_arg), context| {
                let left = try_with_context!(context, left_arg.as_bool());
                let right = try_with_context!(context, right_arg.as_bool());
                (Ok(AmlValue::Boolean(left || right)), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

fn def_l_equal<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefLEqual := 0x93 Operand Operand
     * Operand := TermArg => Integer
     */
    opcode(opcode::DEF_L_EQUAL_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefLEqual",
            term_arg().then(term_arg()).map_with_context(|(left_arg, right_arg), context| {
                let ord = try_with_context!(context, left_arg.cmp(right_arg, context));
                (Ok(AmlValue::Boolean(ord == Ordering::Equal)), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

fn def_l_greater<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefLGreater := 0x94 Operand Operand
     */
    opcode(opcode::DEF_L_GREATER_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefLGreater",
            term_arg().then(term_arg()).map_with_context(|(left_arg, right_arg), context| {
                let ord = try_with_context!(context, left_arg.cmp(right_arg, context));
                (Ok(AmlValue::Boolean(ord == Ordering::Greater)), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

fn def_l_greater_equal<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefLGreaterEqual := LNotOp(0x92) LLessOp(0x95) Operand Operand
     */
    opcode(opcode::DEF_L_NOT_OP)
        .then(opcode(opcode::DEF_L_LESS_OP))
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefLGreaterEqual",
            term_arg().then(term_arg()).map_with_context(|(left_arg, right_arg), context| {
                let ord = try_with_context!(context, left_arg.cmp(right_arg, context));
                (Ok(AmlValue::Boolean(ord != Ordering::Less)), context)
            }),
        ))
        .map(|(((), ()), result)| Ok(result))
}

fn def_l_less<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefLLess := 0x95 Operand Operand
     */
    opcode(opcode::DEF_L_LESS_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefLLess",
            term_arg().then(term_arg()).map_with_context(|(left_arg, right_arg), context| {
                let ord = try_with_context!(context, left_arg.cmp(right_arg, context));
                (Ok(AmlValue::Boolean(ord == Ordering::Less)), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

fn def_l_less_equal<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefLLessEqual := LNotOp(0x92) LGreaterOp(0x94) Operand Operand
     */
    opcode(opcode::DEF_L_NOT_OP)
        .then(opcode(opcode::DEF_L_GREATER_OP))
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefLLessEqual",
            term_arg().then(term_arg()).map_with_context(|(left_arg, right_arg), context| {
                let ord = try_with_context!(context, left_arg.cmp(right_arg, context));
                (Ok(AmlValue::Boolean(ord != Ordering::Greater)), context)
            }),
        ))
        .map(|(((), ()), result)| Ok(result))
}

fn def_l_not_equal<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefLNotEqual := LNotOp(0x92) LEqualOp(0x93) Operand Operand
     */
    opcode(opcode::DEF_L_NOT_OP)
        .then(opcode(opcode::DEF_L_EQUAL_OP))
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefLNotEqual",
            term_arg().then(term_arg()).map_with_context(|(left_arg, right_arg), context| {
                let ord = try_with_context!(context, left_arg.cmp(right_arg, context));
                (Ok(AmlValue::Boolean(ord != Ordering::Equal)), context)
            }),
        ))
        .map(|(((), ()), result)| Ok(result))
}

fn def_mid<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefMid := 0x9e MidObj TermArg TermArg Target
     * MidObj := TermArg => Buffer | String
     */
    opcode(opcode::DEF_MID_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefMid",
            term_arg().then(term_arg()).then(term_arg()).then(target()).map_with_context(
                |(((source, index), length), target), context| {
                    let index = try_with_context!(context, index.as_integer(context)) as usize;
                    let length = try_with_context!(context, length.as_integer(context)) as usize;

                    let result = try_with_context!(
                        context,
                        match source {
                            AmlValue::Buffer(bytes) => {
                                let foo = bytes.lock();
                                if index >= foo.len() {
                                    Ok(AmlValue::Buffer(Arc::new(spinning_top::Spinlock::new(vec![]))))
                                } else if (index + length) >= foo.len() {
                                    Ok(AmlValue::Buffer(Arc::new(spinning_top::Spinlock::new(
                                        foo[index..].to_vec(),
                                    ))))
                                } else {
                                    Ok(AmlValue::Buffer(Arc::new(spinning_top::Spinlock::new(
                                        foo[index..(index + length)].to_vec(),
                                    ))))
                                }
                            }
                            /*
                             * XXX: The spec conflates characters and bytes, so we effectively ignore unicode and do
                             * this bytewise, to hopefully match other implementations.
                             */
                            AmlValue::String(string) => {
                                if index >= string.len() {
                                    Ok(AmlValue::String(String::new()))
                                } else if (index + length) >= string.len() {
                                    Ok(AmlValue::String(string[index..].to_string()))
                                } else {
                                    Ok(AmlValue::String(string[index..(index + length)].to_string()))
                                }
                            }
                            _ => Err(AmlError::TypeCannotBeSliced(source.type_of())),
                        }
                    );

                    try_with_context!(context, context.store(target, result.clone()));
                    (Ok(result), context)
                },
            ),
        ))
        .map(|((), result)| Ok(result))
}

pub fn def_object_type<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefObjectType := 0x8e <SimpleName | DebugObj | DefRefOf | DefDerefOf | DefIndex>
     *
     * Returns an integer representing the type of an AML object. If executed on an object that is a reference to a
     * value (e.g. produced by `Alias`, `RefOf`, or `Index`), the type of the base object is returned. For typeless
     * objects, such as scopes, a type of `0 - Uninitialized` is returned.
     *
     *    0 = Uninitialized
     *    1 = Integer
     *    2 = String
     *    3 = Buffer
     *    4 = Package
     *    5 = Field Unit
     *    6 = Device
     *    7 = Event
     *    8 = Method
     *    9 = Mutex
     *    10 = Operation Region
     *    11 = Power Resource
     *    12 = Processor
     *    13 = Thermal Zone
     *    14 = Buffer Field
     *    15 = Reserved
     *    16 = Debug Object
     *    >16 = *Reserved*
     */
    // TODO: this doesn't correctly handle taking the type of a namespace node (e.g. `\_SB`), but I'm not sure any
    // other implementations do either?
    opcode(opcode::DEF_OBJECT_TYPE_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefObjectType",
            choice!(
                // OSyx: any SuperName, so also `DerefOf (Index (...))`.
                super_name().map_with_context(|target, context| {
                    if matches!(target, crate::name_object::Target::Debug) {
                        return (Ok(AmlValue::Integer(16)), context);
                    }
                    let value = try_with_context!(context, context.target_value(&target));
                    let typ = match value.type_of() {
                        AmlType::Uninitialized => 0,
                        AmlType::Integer => 1,
                        AmlType::String => 2,
                        AmlType::Buffer => 3,
                        AmlType::RawDataBuffer => 3, // TODO: not sure if this is correct
                        AmlType::Package => 4,
                        AmlType::FieldUnit => 5,
                        AmlType::Device => 6,
                        AmlType::Event => 7,
                        AmlType::Method => 8,
                        AmlType::Mutex => 9,
                        AmlType::OpRegion => 10,
                        AmlType::PowerResource => 11,
                        AmlType::Processor => 12,
                        AmlType::ThermalZone => 13,
                        AmlType::BufferField => 14,
                        AmlType::DebugObject => 16,

                        // TODO: what to do with this?
                        AmlType::DdbHandle => 0,
                        // Object references do not have an `ObjectType` of
                        // their own: the referred object's type counts, and
                        // the crate has no reference values, so report the
                        // unknown type instead of aborting the method.
                        AmlType::ObjReference => 0,
                    };

                    (Ok(AmlValue::Integer(typ)), context)
                }),
                debug_obj().map(|()| Ok(AmlValue::Integer(16)))
            ),
        ))
        .map(|((), result)| Ok(result))
}

pub fn def_package<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefPackage := 0x12 PkgLength NumElements PackageElementList
     * NumElements := ByteData
     * PackageElementList := Nothing | <PackageElement PackageElementList>
     * PackageElement := DataRefObject | NameString
     */
    opcode(opcode::DEF_PACKAGE_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefPackage",
            pkg_length().then(take()).feed(|(pkg_length, num_elements)| {
                move |mut input, mut context| {
                    let mut package_contents = Vec::new();

                    while pkg_length.still_parsing(input) {
                        let (new_input, new_context, value) = package_element().parse(input, context)?;
                        input = new_input;
                        context = new_context;

                        package_contents.push(value);
                    }

                    /*
                     * OSyx: NumElements may be larger than the initializer
                     * list (`Package (0x04) {}`, the usual way firmware
                     * declares a package it fills later); the rest of the
                     * elements are uninitialized, here zero. Only a list
                     * longer than NumElements is malformed.
                     */
                    if package_contents.len() > num_elements as usize {
                        return Err((input, context, Propagate::Err(AmlError::MalformedPackage)));
                    }
                    package_contents.resize(num_elements as usize, AmlValue::Integer(0));

                    Ok((input, context, AmlValue::Package(package_contents)))
                }
            }),
        ))
        .map(|((), package)| Ok(package))
}

/// OSyx: `DefVarPackage`, whose element count is computed at run time.
pub fn def_var_package<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefVarPackage := 0x13 PkgLength VarNumElements PackageElementList
     * VarNumElements := TermArg => Integer
     *
     * The count is evaluated for its side effects, but the list is read to
     * the end of the package length: firmware sometimes leaves the count at
     * zero and fills the package anyway.
     */
    opcode(opcode::DEF_VAR_PACKAGE_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefVarPackage",
            pkg_length().feed(|pkg_length| {
                move |mut input: &'a [u8], mut context: &'c mut AmlContext| {
                    let (new_input, new_context, count) = term_arg().parse(input, context)?;
                    input = new_input;
                    context = new_context;
                    if let Err(err) = count.as_integer(context) {
                        return Err((input, context, Propagate::Err(err)));
                    }

                    let count = count.as_integer(context).unwrap_or(0) as usize;
                    let mut contents = Vec::new();
                    while pkg_length.still_parsing(input) {
                        let (new_input, new_context, value) = package_element().parse(input, context)?;
                        input = new_input;
                        context = new_context;
                        contents.push(value);
                    }
                    // The elements the list does not give are uninitialized (zero).
                    if contents.len() < count && count <= 0x1_0000 {
                        contents.resize(count, AmlValue::Integer(0));
                    }

                    Ok((input, context, AmlValue::Package(contents)))
                }
            }),
        ))
        .map(|((), package)| Ok(package))
}

pub fn package_element<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    // OSyx: firmware builds the `_BST` package from locals and arguments
    // (`Package () { Local0, Local1, ... }`); they are taken by value.
    choice!(
        data_ref_object(),
        crate::misc::local_obj().map_with_context(|n, context| {
            (Ok(try_with_context!(context, context.local(n)).clone()), context)
        }),
        crate::misc::arg_obj().map_with_context(|n, context| {
            (Ok(try_with_context!(context, context.current_arg(n)).clone()), context)
        }),
        name_string().map(|string| Ok(AmlValue::String(string.as_string())))
    )
}

/// OSyx: the arithmetic and logic operators of ACPI 19.6 that firmware
/// battery and EC methods use (the interpreter had Add, And and the shifts).
macro_rules! binary_operator {
    ($fn_name:ident, $opcode:expr, $label:literal, |$l:ident, $r:ident| $body:expr) => {
        fn $fn_name<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
        where
            'c: 'a,
        {
            opcode($opcode)
                .then(comment_scope(
                    DebugVerbosity::AllScopes,
                    $label,
                    term_arg().then(term_arg()).then(target()).map_with_context(
                        |((left_arg, right_arg), target), context| {
                            let $l = try_with_context!(context, left_arg.as_integer(context));
                            let $r = try_with_context!(context, right_arg.as_integer(context));
                            let result = AmlValue::Integer($body);
                            try_with_context!(context, context.store(target, result.clone()));
                            (Ok(result), context)
                        },
                    ),
                ))
                .map(|((), result)| Ok(result))
        }
    };
}

binary_operator!(def_subtract, opcode::DEF_SUBTRACT_OP, "DefSubtract", |l, r| l.wrapping_sub(r));
binary_operator!(def_multiply, opcode::DEF_MULTIPLY_OP, "DefMultiply", |l, r| l.wrapping_mul(r));
binary_operator!(def_or, opcode::DEF_OR_OP, "DefOr", |l, r| l | r);
binary_operator!(def_xor, opcode::DEF_XOR_OP, "DefXOr", |l, r| l ^ r);
binary_operator!(def_nand, opcode::DEF_NAND_OP, "DefNAnd", |l, r| !(l & r));
binary_operator!(def_nor, opcode::DEF_NOR_OP, "DefNOr", |l, r| !(l | r));

fn def_mod<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefMod := 0x85 Dividend Divisor Target
     */
    opcode(opcode::DEF_MOD_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefMod",
            term_arg().then(term_arg()).then(target()).map_with_context(|((dividend, divisor), target), context| {
                let dividend = try_with_context!(context, dividend.as_integer(context));
                let divisor = try_with_context!(context, divisor.as_integer(context));
                let result = AmlValue::Integer(try_with_context!(
                    context,
                    dividend.checked_rem(divisor).ok_or(AmlError::FieldInvalidAccessSize)
                ));
                try_with_context!(context, context.store(target, result.clone()));
                (Ok(result), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

fn def_divide<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefDivide := 0x78 Dividend Divisor Remainder Quotient
     * The result is the quotient.
     */
    opcode(opcode::DEF_DIVIDE_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefDivide",
            term_arg().then(term_arg()).then(target()).then(target()).map_with_context(
                |(((dividend, divisor), remainder), quotient), context| {
                    let dividend = try_with_context!(context, dividend.as_integer(context));
                    let divisor = try_with_context!(context, divisor.as_integer(context));
                    if divisor == 0 {
                        return (Err(Propagate::Err(AmlError::FieldInvalidAccessSize)), context);
                    }
                    let q = AmlValue::Integer(dividend / divisor);
                    try_with_context!(context, context.store(remainder, AmlValue::Integer(dividend % divisor)));
                    try_with_context!(context, context.store(quotient, q.clone()));
                    (Ok(q), context)
                },
            ),
        ))
        .map(|((), result)| Ok(result))
}

fn def_not<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefNot := 0x80 Operand Target
     */
    opcode(opcode::DEF_NOT_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefNot",
            term_arg().then(target()).map_with_context(|(operand, target), context| {
                let operand = try_with_context!(context, operand.as_integer(context));
                let result = AmlValue::Integer(!operand);
                try_with_context!(context, context.store(target, result.clone()));
                (Ok(result), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

fn def_l_not<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefLNot := 0x92 Operand (tried after LNotEqual, LGreaterEqual and LLessEqual)
     */
    opcode(opcode::DEF_L_NOT_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefLNot",
            term_arg().map_with_context(|operand, context| {
                let operand = try_with_context!(context, operand.as_bool());
                (Ok(AmlValue::Boolean(!operand)), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

fn def_size_of<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefSizeOf := 0x87 SuperName
     */
    opcode(opcode::DEF_SIZE_OF_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefSizeOf",
            super_name().map_with_context(|target, context| {
                let value = try_with_context!(context, context.target_value(&target));
                let size = match value {
                    AmlValue::Buffer(b) => b.lock().len(),
                    AmlValue::String(s) => s.len(),
                    AmlValue::Package(p) => p.len(),
                    _ => return (Err(Propagate::Err(AmlError::FieldInvalidAccessSize)), context),
                };
                (Ok(AmlValue::Integer(size as u64)), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

fn def_index<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefIndex := 0x88 BuffPkgStrObj IndexValue Target
     * The element is returned by value (not as a reference).
     */
    opcode(opcode::DEF_INDEX_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefIndex",
            term_arg().then(term_arg()).then(target()).map_with_context(|((object, position), target), context| {
                let position = try_with_context!(context, position.as_integer(context)) as usize;
                let element = match object {
                    AmlValue::Package(items) => items.get(position).cloned(),
                    AmlValue::Buffer(b) => b.lock().get(position).map(|&byte| AmlValue::Integer(byte as u64)),
                    AmlValue::String(s) => s.as_bytes().get(position).map(|&byte| AmlValue::Integer(byte as u64)),
                    _ => None,
                };
                let Some(element) = element else {
                    return (Err(Propagate::Err(AmlError::FieldInvalidAccessSize)), context);
                };
                try_with_context!(context, context.store(target, element.clone()));
                (Ok(element), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

fn def_deref_of<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefDerefOf := 0x83 ObjReference
     * `Index` already yields the element itself, so there is nothing to follow.
     */
    opcode(opcode::DEF_DEREF_OF_OP)
        .then(comment_scope(DebugVerbosity::AllScopes, "DefDerefOf", term_arg()))
        .map(|((), value)| Ok(value))
}

fn def_shift_left<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefShiftLeft := 0x79 Operand ShiftCount Target
     * Operand := TermArg => Integer
     * ShiftCount := TermArg => Integer
     */
    opcode(opcode::DEF_SHIFT_LEFT)
        .then(comment_scope(DebugVerbosity::Scopes, "DefShiftLeft", term_arg().then(term_arg()).then(target())))
        .map_with_context(|((), ((operand, shift_count), target)), context| {
            let operand = try_with_context!(context, operand.as_integer(context));
            let shift_count = try_with_context!(context, shift_count.as_integer(context));
            // OSyx: shifting by the integer width or more leaves zero.
            let result = AmlValue::Integer(TryInto::<u32>::try_into(shift_count).ok().and_then(|n| operand.checked_shl(n)).unwrap_or(0));

            try_with_context!(context, context.store(target, result.clone()));
            (Ok(result), context)
        })
}

fn def_shift_right<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefShiftRight := 0x7a Operand ShiftCount Target
     * Operand := TermArg => Integer
     * ShiftCount := TermArg => Integer
     */
    opcode(opcode::DEF_SHIFT_RIGHT)
        .then(comment_scope(DebugVerbosity::Scopes, "DefShiftRight", term_arg().then(term_arg()).then(target())))
        .map_with_context(|((), ((operand, shift_count), target)), context| {
            let operand = try_with_context!(context, operand.as_integer(context));
            let shift_count = try_with_context!(context, shift_count.as_integer(context));
            let result = AmlValue::Integer(TryInto::<u32>::try_into(shift_count).ok().and_then(|n| operand.checked_shr(n)).unwrap_or(0));

            try_with_context!(context, context.store(target, result.clone()));
            (Ok(result), context)
        })
}

fn def_store<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefStore := 0x70 TermArg SuperName
     *
     * Implicit conversion is only applied when the destination target is a `Name` - not when we
     * are storing into a method local or argument (these stores are semantically identical to
     * CopyObject). We must also make sure to return a copy of the data that is in the destination
     * after the store (as opposed to the data we think we put into it), because some stores can
     * alter the data during the store.
     */
    opcode(opcode::DEF_STORE_OP)
        .then(comment_scope(DebugVerbosity::Scopes, "DefStore", term_arg().then(super_name())))
        .map_with_context(|((), (value, target)), context| {
            (Ok(try_with_context!(context, context.store(target, value))), context)
        })
}

fn def_to_integer<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefToInteger := 0x99 Operand Target
     * Operand := TermArg
     */
    opcode(opcode::DEF_TO_INTEGER_OP)
        .then(comment_scope(DebugVerbosity::AllScopes, "DefToInteger", term_arg().then(target())))
        .map_with_context(|((), (operand, target)), context| {
            let result = match operand {
                AmlValue::Integer(value) => AmlValue::Integer(value),
                AmlValue::Buffer(data) => {
                    AmlValue::Integer(try_with_context!(context, AmlValue::Buffer(data).as_integer(context)))
                }
                AmlValue::String(string) => AmlValue::Integer(try_with_context!(
                    context,
                    if string.starts_with("0x") {
                        u64::from_str_radix(string.trim_start_matches("0x"), 16)
                    } else {
                        string.parse::<u64>()
                    }
                    .map_err(|_| AmlError::IncompatibleValueConversion {
                        current: AmlType::String,
                        target: AmlType::Integer,
                    })
                )),
                _ => {
                    return (
                        Err(Propagate::Err(AmlError::IncompatibleValueConversion {
                            current: operand.type_of(),
                            target: AmlType::Integer,
                        })),
                        context,
                    )
                }
            };
            try_with_context!(context, context.store(target, result.clone()));
            (Ok(result), context)
        })
}

/// OSyx: the bytes `DefToBuffer` produces. An integer is copied as its
/// eight little-endian bytes; a string keeps its terminating null.
fn to_buffer_bytes(value: &AmlValue, context: &AmlContext) -> Result<Vec<u8>, AmlError> {
    match value {
        AmlValue::Buffer(bytes) => Ok(bytes.lock().clone()),
        AmlValue::Integer(value) => Ok(value.to_le_bytes().to_vec()),
        AmlValue::Boolean(value) => Ok((*value as u64).to_le_bytes().to_vec()),
        AmlValue::String(string) => {
            let mut bytes = Vec::with_capacity(string.len() + 1);
            bytes.extend_from_slice(string.as_bytes());
            bytes.push(0);
            Ok(bytes)
        }
        AmlValue::Field { .. } | AmlValue::BufferField { .. } => {
            Ok(value.as_integer(context)?.to_le_bytes().to_vec())
        }
        _ => Err(AmlError::IncompatibleValueConversion { current: value.type_of(), target: AmlType::Buffer }),
    }
}

fn def_to_buffer<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefToBuffer := 0x96 Operand Target
     * Operand := TermArg => Buffer | Integer | String
     */
    opcode(opcode::DEF_TO_BUFFER_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefToBuffer",
            term_arg().then(target()).map_with_context(|(operand, target), context| {
                let bytes = try_with_context!(context, to_buffer_bytes(&operand, context));
                let result = AmlValue::Buffer(Arc::new(spinning_top::Spinlock::new(bytes)));
                try_with_context!(context, context.store(target, result.clone()));
                (Ok(result), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

fn to_string_value(value: &AmlValue, length: usize, context: &AmlContext) -> Result<String, AmlError> {
    match value {
        AmlValue::String(string) => Ok(string.clone()),
        AmlValue::Integer(value) => Ok(value.to_string()),
        AmlValue::Boolean(value) => Ok((*value as u64).to_string()),
        AmlValue::Buffer(bytes) => {
            /*
             * The length argument applies to buffers only: it is the maximum
             * number of characters, and the string ends at the first null.
             */
            let bytes = bytes.lock();
            let bytes = &bytes[..bytes.len().min(length)];
            let end = bytes.iter().position(|&byte| byte == 0).unwrap_or(bytes.len());
            Ok(bytes[..end].iter().map(|&byte| byte as char).collect())
        }
        AmlValue::Field { .. } | AmlValue::BufferField { .. } => {
            Ok(value.as_integer(context)?.to_string())
        }
        _ => Err(AmlError::IncompatibleValueConversion { current: value.type_of(), target: AmlType::String }),
    }
}

fn def_to_string<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefToString := 0x9C Operand Length Target
     */
    opcode(opcode::DEF_TO_STRING_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefToString",
            term_arg().then(term_arg()).then(target()).map_with_context(
                |((operand, length), target), context| {
                    let length = try_with_context!(context, length.as_integer(context)) as usize;
                    let string = try_with_context!(context, to_string_value(&operand, length, context));
                    let result = AmlValue::String(string);
                    try_with_context!(context, context.store(target, result.clone()));
                    (Ok(result), context)
                },
            ),
        ))
        .map(|((), result)| Ok(result))
}

fn def_to_decimal_string<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefToDecimalString := 0x97 Operand Target
     */
    opcode(opcode::DEF_TO_DECIMAL_STRING_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefToDecimalString",
            term_arg().then(target()).map_with_context(|(operand, target), context| {
                let value = try_with_context!(context, operand.as_integer(context));
                let result = AmlValue::String(format!("{value}"));
                try_with_context!(context, context.store(target, result.clone()));
                (Ok(result), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

fn def_to_hex_string<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefToHexString := 0x98 Operand Target
     */
    opcode(opcode::DEF_TO_HEX_STRING_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefToHexString",
            term_arg().then(target()).map_with_context(|(operand, target), context| {
                let value = try_with_context!(context, operand.as_integer(context));
                let result = AmlValue::String(format!("0x{value:X}"));
                try_with_context!(context, context.store(target, result.clone()));
                (Ok(result), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

fn def_copy_object<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefCopyObject := 0x9D TermArg SimpleName
     */
    opcode(opcode::DEF_COPY_OBJECT_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefCopyObject",
            term_arg().then(simple_name()).map_with_context(|(value, target), context| {
                let stored = try_with_context!(context, context.store(target, value.clone()));
                (Ok(stored), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

fn def_acquire<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefAcquire := ExtOpPrefix 0x23 MutexObject Timeout
     *
     * The interpreter runs firmware on one thread, so a mutex is never held
     * by anyone else: acquiring it always succeeds.
     */
    ext_opcode(opcode::EXT_DEF_ACQUIRE_OP)
        .then(comment_scope(
            DebugVerbosity::Scopes,
            "DefAcquire",
            // OSyx: the timeout is a WordData (`0xFFFF` in nearly every
            // firmware), not a TermArg: read as one it left a stray byte.
            super_name().then(take_u16()).map(|(_mutex, _timeout)| Ok(AmlValue::Integer(0))),
        ))
        .map(|((), result)| Ok(result))
}

fn def_wait<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefWait := ExtOpPrefix 0x25 EventObject Timeout
     *
     * Events are never signalled (nothing in the interpreter runs
     * concurrently), and a wait that timed out would make firmware take its
     * error path; report success so the object is treated as ready.
     */
    ext_opcode(opcode::EXT_DEF_WAIT_OP)
        .then(comment_scope(
            DebugVerbosity::Scopes,
            "DefWait",
            super_name().then(term_arg()).map(|(_event, _timeout)| Ok(AmlValue::Integer(0))),
        ))
        .map(|((), result)| Ok(result))
}

/// OSyx: `RefOf`. The interpreter has no reference objects; the object
/// itself stands in for a reference to it, and `DerefOf` passes it through.
fn def_ref_of<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefRefOf := 0x71 SuperName
     */
    opcode(opcode::DEF_REF_OF_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefRefOf",
            super_name().map_with_context(|target, context| {
                let value = try_with_context!(context, context.target_value(&target));
                (Ok(try_with_context!(context, value.resolved(context))), context)
            }),
        ))
        .map(|((), result)| Ok(result))
}

/// OSyx: one-operand integer operators with a target.
macro_rules! unary_operator {
    ($fn_name:ident, $opcode:expr, $label:literal, |$v:ident| $body:expr) => {
        fn $fn_name<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
        where
            'c: 'a,
        {
            $opcode
                .then(comment_scope(
                    DebugVerbosity::AllScopes,
                    $label,
                    term_arg().then(target()).map_with_context(|(operand, target), context| {
                        let $v = try_with_context!(context, operand.as_integer(context));
                        let result = AmlValue::Integer($body);
                        try_with_context!(context, context.store(target, result.clone()));
                        (Ok(result), context)
                    }),
                ))
                .map(|((), result)| Ok(result))
        }
    };
}

// The position of the most and the least significant set bit, counted from 1; 0 for zero.
unary_operator!(def_find_set_left_bit, opcode(opcode::DEF_FIND_SET_LEFT_BIT_OP), "DefFindSetLeftBit", |v| {
    64 - v.leading_zeros() as u64
});
unary_operator!(def_find_set_right_bit, opcode(opcode::DEF_FIND_SET_RIGHT_BIT_OP), "DefFindSetRightBit", |v| {
    if v == 0 { 0 } else { v.trailing_zeros() as u64 + 1 }
});
unary_operator!(def_from_bcd, ext_opcode(opcode::EXT_DEF_FROM_BCD_OP), "DefFromBCD", |v| {
    (0..16).rev().fold(0u64, |acc, digit| acc.wrapping_mul(10).wrapping_add((v >> (digit * 4)) & 0xf))
});
unary_operator!(def_to_bcd, ext_opcode(opcode::EXT_DEF_TO_BCD_OP), "DefToBCD", |v| {
    let mut rest = v;
    let mut bcd = 0u64;
    for digit in 0..16 {
        bcd |= (rest % 10) << (digit * 4);
        rest /= 10;
    }
    bcd
});

/// OSyx: `Match`: the index of the first package element, from `StartIndex`
/// on, that satisfies both comparisons; `Ones` when there is none.
fn def_match<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefMatch := 0x89 SearchPkg MatchOpcode Operand MatchOpcode Operand StartIndex
     * MatchOpcode := ByteData (MTR 0, MEQ 1, MLE 2, MLT 3, MGE 4, MGT 5)
     */
    fn holds(op: u8, element: u64, operand: u64) -> bool {
        match op {
            0 => true,
            1 => element == operand,
            2 => element <= operand,
            3 => element < operand,
            4 => element >= operand,
            5 => element > operand,
            _ => false,
        }
    }
    opcode(opcode::DEF_MATCH_OP)
        .then(comment_scope(
            DebugVerbosity::AllScopes,
            "DefMatch",
            term_arg().then(take()).then(term_arg()).then(take()).then(term_arg()).then(term_arg()).map_with_context(
                |(((((package, op1), operand1), op2), operand2), start), context| {
                    let AmlValue::Package(items) = package else {
                        return (
                            Err(Propagate::Err(AmlError::IncompatibleValueConversion {
                                current: package.type_of(),
                                target: AmlType::Package,
                            })),
                            context,
                        );
                    };
                    let operand1 = try_with_context!(context, operand1.as_integer(context));
                    let operand2 = try_with_context!(context, operand2.as_integer(context));
                    let start = try_with_context!(context, start.as_integer(context)) as usize;
                    let found = items.iter().enumerate().skip(start).find_map(|(i, item)| {
                        // Elements that are not integers never match.
                        let element = item.as_integer(context).ok()?;
                        (holds(op1, element, operand1) && holds(op2, element, operand2)).then_some(i as u64)
                    });
                    (Ok(AmlValue::Integer(found.unwrap_or(u64::MAX))), context)
                },
            ),
        ))
        .map(|((), result)| Ok(result))
}

/// OSyx: `Timer`: a monotonic count of 100 ns units.
fn def_timer<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    ext_opcode(opcode::EXT_DEF_TIMER_OP)
        .map_with_context(|(), context| (Ok(AmlValue::Integer(context.handler.timer_100ns())), context))
}

/// OSyx: `LoadTable` names a table for the host to load; none is loaded
/// from inside a method, and zero ("no table") is the documented result.
fn def_load_table<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * DefLoadTable := ExtOpPrefix 0x1F TermArg TermArg TermArg TermArg TermArg TermArg
     */
    ext_opcode(opcode::EXT_DEF_LOAD_TABLE_OP)
        .then(comment_scope(DebugVerbosity::AllScopes, "DefLoadTable", n_of(term_arg(), 6)))
        .map(|((), _)| Ok(AmlValue::Integer(0)))
}

fn method_invocation<'a, 'c>() -> impl Parser<'a, 'c, AmlValue>
where
    'c: 'a,
{
    /*
     * MethodInvocation := NameString TermArgList
     *
     * MethodInvocation is the worst of the AML structures, because you're meant to figure out how much you're
     * meant to parse using the name of the method (by knowing from its definition how how many arguments it
     * takes). However, the definition of a method can in theory appear after an invocation of that method, and
     * so parsing them properly can be very difficult.
     * NOTE: We don't support the case of the definition appearing after the invocation.
     */
    comment_scope(
        DebugVerbosity::Scopes,
        "MethodInvocation",
        name_string()
            .map_with_context(move |name, context| {
                let (full_path, handle) =
                    try_with_context!(context, context.namespace.search(&name, &context.current_scope)).clone();

                /*
                 * `None` if the path is not a method and so doesn't have arguments, or `Some(the number of
                 * arguments to parse)` if it's a method.
                 */
                let num_args = if let AmlValue::Method { flags, .. } =
                    try_with_context!(context, context.namespace.get(handle))
                {
                    Some(flags.arg_count())
                } else {
                    None
                };
                (Ok((full_path, num_args)), context)
            })
            .feed(|(path, num_args)| {
                move |mut input: &'a [u8], mut context: &'c mut AmlContext| -> ParseResult<'a, 'c, AmlValue> {
                    let Some(num_args) = num_args else {
                        // OSyx: not a method: the object's value (a field is read now).
                        let value = match context.namespace.get_by_path(&path).cloned().and_then(|v| v.resolved(context)) {
                            Ok(value) => value,
                            Err(err) => return Err((input, context, Propagate::Err(err))),
                        };
                        return Ok((input, context, value));
                    };
                    /*
                     * OSyx: the arguments, each with where it came from: a
                     * package argument is the caller's package (see
                     * `ArgSource`), so the callee's element stores must
                     * reach it.
                     */
                    let mut arg_list = Vec::with_capacity(num_args as usize);
                    let mut sources = [crate::ArgSource::default(); 7];
                    for i in 0..num_args as usize {
                        let start = input;
                        let (new_input, new_context, value) = match term_arg().parse(input, context) {
                            Ok(parsed) => parsed,
                            Err((_, context, propagate)) => return Err((input, context, propagate)),
                        };
                        input = new_input;
                        context = new_context;
                        let consumed = start.len() - input.len();
                        let first = start.first().copied().unwrap_or(0);
                        let name = if matches!(value, AmlValue::Package(_)) && !matches!(first, 0x60..=0x6e) {
                            match name_string().parse(start, context) {
                                Ok((after, new_context, name)) => {
                                    context = new_context;
                                    (start.len() - after.len() == consumed).then_some(name)
                                }
                                Err((_, new_context, _)) => {
                                    context = new_context;
                                    None
                                }
                            }
                        } else {
                            None
                        };
                        if i < 7 {
                            sources[i] = context.arg_source(first, consumed, name.as_ref(), &value);
                        }
                        arg_list.push(value);
                    }
                    let args = match Args::from_list(arg_list) {
                        Ok(args) => args,
                        Err(err) => return Err((input, context, Propagate::Err(err))),
                    };
                    match context.invoke_method_from(&path, args, sources) {
                        Ok(value) => Ok((input, context, value)),
                        Err(err) => Err((input, context, Propagate::Err(err))),
                    }
                }
            }),
    )
}
