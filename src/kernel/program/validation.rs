//! Resolve native signatures and prove local bounds/link/dataflow before wiring.

use crate::{
    ir::{Binary, Type},
    kernel::appliance::Native,
};
use lattice_peer_wire::program::{
    Definition, Error, Link, Location, Operand, Operation, Region, NONE,
};

pub(super) const MAX_RECORDS: usize = 128;

pub(super) fn validate(bytes: &[u8], native: &dyn Native, surface: &[u8; 32]) -> Result<(), Error> {
    let d = Definition::parse(bytes)?;
    if d.surface() != surface {
        return Err(Error::Compatibility);
    }
    let mut offsets = [NONE; MAX_RECORDS];
    let mut edges = [[NONE; 2]; MAX_RECORDS];
    let mut outputs = [None; MAX_RECORDS];
    let mut entries = [false; MAX_RECORDS];
    let mut count = 0;
    let operand = |o: Operand| match o {
        Operand::Location(l) => d.check_location(l),
        _ => Ok(()),
    };
    for item in d.records() {
        let r = item?;
        if count == MAX_RECORDS {
            return Err(Error::Capacity);
        }
        offsets[count] = r.offset;
        let output = match r.operation {
            Operation::Read { output, .. }
            | Operation::Unary { output, .. }
            | Operation::Binary { output, .. } => Some(output),
            _ => None,
        };
        if let Some(output) = output {
            d.check_location(output)?;
        }
        outputs[count] = output;
        match r.operation {
            Operation::Temporal { entry, .. } => {
                if entry >= r.offset {
                    return Err(Error::Link);
                }
                activation(d, entry)?;
            }
            Operation::Entry {
                upstream, inputs, ..
            } => {
                entries[count] = true;
                if d.head() == Some(r.offset) && upstream != Link::Stop {
                    return Err(Error::Link);
                }
                if d.head() == Some(r.offset) && !inputs.is_empty() {
                    return Err(Error::Type);
                }
                if let Link::Local(target) = upstream {
                    if target >= r.offset
                        || !matches!(d.record(target)?.operation, Operation::Entry { .. })
                    {
                        return Err(Error::Link);
                    }
                }
                for (i, input) in inputs.iter().enumerate() {
                    let Operand::Location(l) = input else {
                        return Err(Error::Type);
                    };
                    if l.region != Region::Carried {
                        return Err(Error::Type);
                    }
                    d.check_location(l)?;
                    if inputs
                        .iter()
                        .take(i)
                        .any(|previous| matches!(previous, Operand::Location(p) if overlaps(p, l)))
                    {
                        return Err(Error::Type);
                    }
                }
            }
            Operation::Read { state, output, .. } => {
                if native.state_binding(state).map(|s| s.0) != Some(output.ty) {
                    return Err(Error::Binding);
                }
            }
            Operation::Unary {
                operation,
                input,
                output,
                ..
            } => {
                operand(input)?;
                if operation.result_type(input.ty()) != Ok(output.ty) {
                    return Err(Error::Type);
                }
            }
            Operation::Binary {
                operation,
                left,
                right,
                output,
                ..
            } => {
                operand(left)?;
                operand(right)?;
                if operation.result_type(left.ty(), right.ty()) != Ok(output.ty) {
                    return Err(Error::Type);
                }
            }
            Operation::Test {
                operation,
                left,
                right,
                ..
            } => {
                operand(left)?;
                operand(right)?;
                if !matches!(
                    operation,
                    Binary::Equal
                        | Binary::NotEqual
                        | Binary::Less
                        | Binary::LessEqual
                        | Binary::Greater
                        | Binary::GreaterEqual
                ) || operation.result_type(left.ty(), right.ty()) != Ok(Type::Bool)
                {
                    return Err(Error::Type);
                }
            }
            Operation::Branch { input, .. } => {
                operand(input)?;
                if input.ty() != Type::Bool {
                    return Err(Error::Type);
                }
            }
            Operation::Invoke {
                transformer,
                arguments,
                ..
            } => {
                let types = native
                    .transformer_signature(transformer)
                    .ok_or(Error::Binding)?;
                if arguments.len() != types.len() {
                    return Err(Error::Type);
                }
                for (argument, ty) in arguments.iter().zip(types) {
                    operand(argument)?;
                    if argument.ty() != *ty {
                        return Err(Error::Type);
                    }
                }
            }
        }
        for (edge, link) in edges[count].iter_mut().zip(r.operation.successors()) {
            if let Link::Local(target) = link {
                // The compiler emits local records in topological order. Entries
                // are external arrival points, not jumps that reset local scratch.
                if target <= r.offset
                    || matches!(d.record(target)?.operation, Operation::Entry { .. })
                {
                    return Err(Error::Link);
                }
                *edge = target;
            }
        }
        count += 1;
    }
    if let Some(head) = d.head() {
        if !matches!(
            d.record(head)?.operation,
            Operation::Entry {
                upstream: Link::Stop,
                ..
            }
        ) {
            return Err(Error::Link);
        }
    }
    let mut reached = entries;
    for i in 0..count {
        if !reached[i] {
            return Err(Error::Link);
        }
        for edge in edges[i] {
            if edge != NONE {
                let target = offsets[..count]
                    .iter()
                    .position(|o| *o == edge)
                    .ok_or(Error::Link)?;
                reached[target] = true;
            }
        }
    }
    for source in d.sources() {
        if native.state_binding(source.state).is_none() {
            return Err(Error::Binding);
        }
        activation(d, source.entry)?;
        if d.sources()
            .take_while(|s| s.offset < source.offset)
            .any(|s| s.state == source.state)
        {
            return Err(Error::Binding);
        }
    }
    // A location is readable only if every incoming local path initialized that
    // exact typed value. Scratch starts empty at every external entry. Temporary
    // arrays belong to validation, not a resident entity/edge registry.
    let check = |target: usize, value: Operand| -> Result<(), Error> {
        let Operand::Location(location) = value else {
            return Ok(());
        };
        let mut valid = [false; MAX_RECORDS];
        for i in 0..=target {
            let incoming = if entries[i] {
                let Operation::Entry { inputs, .. } = d.record(offsets[i])?.operation else {
                    unreachable!()
                };
                inputs.iter().any(|v| v == Operand::Location(location))
            } else {
                let mut found = false;
                let mut all = true;
                for j in 0..i {
                    if edges[j].contains(&offsets[i]) {
                        found = true;
                        all &= valid[j];
                    }
                }
                found && all
            };
            if i == target {
                return if incoming { Ok(()) } else { Err(Error::Type) };
            }
            valid[i] = match outputs[i] {
                Some(output) if overlaps(output, location) => output == location,
                _ => incoming,
            };
        }
        unreachable!()
    };
    for (i, item) in d.records().enumerate() {
        match item?.operation {
            Operation::Temporal { entry, .. } => {
                let mut from_entry = [false; MAX_RECORDS];
                let entry_index = offsets[..count]
                    .iter()
                    .position(|o| *o == entry)
                    .ok_or(Error::Link)?;
                from_entry[entry_index] = true;
                for j in entry_index..i {
                    if from_entry[j] {
                        for edge in edges[j] {
                            if edge != NONE {
                                let k = offsets[..count]
                                    .iter()
                                    .position(|o| *o == edge)
                                    .ok_or(Error::Link)?;
                                from_entry[k] = true;
                            }
                        }
                    }
                }
                if !from_entry[i] {
                    return Err(Error::Link);
                }
            }
            Operation::Unary { input, .. } | Operation::Branch { input, .. } => check(i, input)?,
            Operation::Binary { left, right, .. } | Operation::Test { left, right, .. } => {
                check(i, left)?;
                check(i, right)?;
            }
            Operation::Invoke { arguments, .. } => {
                for a in arguments.iter() {
                    check(i, a)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}
fn activation(d: Definition<'_>, mut at: u16) -> Result<(), Error> {
    loop {
        let Operation::Entry { upstream, .. } = d.record(at)?.operation else {
            return Err(Error::Link);
        };
        match upstream {
            Link::Stop if d.head() == Some(at) => return Ok(()),
            Link::Remote(_) => return Ok(()),
            Link::Local(previous) if previous < at => at = previous,
            _ => return Err(Error::Link),
        }
    }
}
fn overlaps(a: Location, b: Location) -> bool {
    a.region == b.region && a.offset < b.end().unwrap() && b.offset < a.end().unwrap()
}
