// Copyright (C) 2013-2020 Blockstack PBC, a public benefit corporation
// Copyright (C) 2020 Stacks Open Internet Foundation
//
// This program is free software: you can redistribute it and/or modify
// it under the terms of the GNU General Public License as published by
// the Free Software Foundation, either version 3 of the License, or
// (at your option) any later version.
//
// This program is distributed in the hope that it will be useful,
// but WITHOUT ANY WARRANTY; without even the implied warranty of
// MERCHANTABILITY or FITNESS FOR A PARTICULAR PURPOSE.  See the
// GNU General Public License for more details.
//
// You should have received a copy of the GNU General Public License
// along with this program.  If not, see <http://www.gnu.org/licenses/>.

use vm::costs::cost_functions::ClarityCostFunction;
use vm::costs::runtime_cost;
use vm::errors::{check_argument_count, CheckErrors, InterpreterResult as Result};
use vm::representations::SymbolicExpression;
use vm::types::SequenceSubtype::{BufferType, StringType};
use vm::types::StringSubtype::ASCII;
use vm::types::TypeSignature::SequenceType;
use vm::types::{
    ASCIIData, BuffData, BufferLength, CharType, SequenceData, TypeSignature, UTF8Data, Value,
};
use vm::{apply, eval, lookup_function, Environment, LocalContext};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum EndianDirection {
    LittleEndian,
    BigEndian,
}

// The functions in this file support conversion from (buff 16) to either 1) int or 2) uint,
// from formats 1) big-endian and 2) little-endian.
//
// The function 'buff_to_int_generic' describes the logic common to these four functions.
// This is a generic function for conversion from a buffer to an int or uint. The four
// versions of Clarity function each call this, with different values for 'conversion_fn'.
//
// This function checks and parses the arguments, and calls 'conversion_fn' to do
// the specific form of conversion required.
pub fn buff_to_int_generic(
    value: Value,
    direction: EndianDirection,
    conversion_fn: fn([u8; 16]) -> Value,
) -> Result<Value> {
    match value {
        Value::Sequence(SequenceData::Buffer(ref sequence_data)) => {
            if sequence_data.len() > BufferLength(16) {
                return Err(CheckErrors::TypeValueError(
                    SequenceType(BufferType(BufferLength(16))),
                    value,
                )
                .into());
            } else {
                let mut transfer_buffer = [0u8; 16];
                let mut original_slice = sequence_data.as_slice();
                // 'conversion_fn' expects to receive a 16-byte buffer. If the input is little-endian, it should
                // be zero-padded on the right. If the input is big-endian, it should be zero-padded on the left.
                let offset = if direction == EndianDirection::LittleEndian {
                    0
                } else {
                    transfer_buffer.len() - original_slice.len()
                };
                for from_index in 0..original_slice.len() {
                    let to_index = from_index + offset;
                    transfer_buffer[to_index] = original_slice[from_index];
                }
                let value = conversion_fn(transfer_buffer);
                return Ok(value);
            }
        }
        _ => {
            return Err(CheckErrors::TypeValueError(
                SequenceType(BufferType(BufferLength(16))),
                value,
            )
            .into())
        }
    };
}

pub fn native_buff_to_int_le(value: Value) -> Result<Value> {
    fn convert_to_int_le(buffer: [u8; 16]) -> Value {
        let value = i128::from_le_bytes(buffer);
        return Value::Int(value);
    }
    return buff_to_int_generic(value, EndianDirection::LittleEndian, convert_to_int_le);
}

pub fn native_buff_to_uint_le(value: Value) -> Result<Value> {
    fn convert_to_uint_le(buffer: [u8; 16]) -> Value {
        let value = u128::from_le_bytes(buffer);
        return Value::UInt(value);
    }

    return buff_to_int_generic(value, EndianDirection::LittleEndian, convert_to_uint_le);
}

pub fn native_buff_to_int_be(value: Value) -> Result<Value> {
    fn convert_to_int_be(buffer: [u8; 16]) -> Value {
        let value = i128::from_be_bytes(buffer);
        return Value::Int(value);
    }
    return buff_to_int_generic(value, EndianDirection::BigEndian, convert_to_int_be);
}

pub fn native_buff_to_uint_be(value: Value) -> Result<Value> {
    fn convert_to_uint_be(buffer: [u8; 16]) -> Value {
        let value = u128::from_be_bytes(buffer);
        return Value::UInt(value);
    }
    return buff_to_int_generic(value, EndianDirection::BigEndian, convert_to_uint_be);
}

pub fn native_string_to_int_generic(
    value: Value,
    conversion_fn: fn(String) -> Result<Value>,
) -> Result<Value> {
    match value {
        Value::Sequence(SequenceData::String(CharType::ASCII(ASCIIData { data }))) => {
            match String::from_utf8(data) {
                Ok(as_string) => conversion_fn(as_string),
                Err(_error) => Ok(Value::none()),
            }
        }
        Value::Sequence(SequenceData::String(CharType::UTF8(UTF8Data { data }))) => {
            let flattened_bytes = data.into_iter().flatten().collect();
            match String::from_utf8(flattened_bytes) {
                Ok(as_string) => conversion_fn(as_string),
                Err(_error) => Ok(Value::none()),
            }
        }
        _ => Err(CheckErrors::UnionTypeValueError(
            vec![
                TypeSignature::max_string_ascii(),
                TypeSignature::max_string_utf8(),
            ],
            value,
        )
        .into()),
    }
}

fn safe_convert_string_to_int(raw_string: String) -> Result<Value> {
    let possible_int = raw_string.parse::<i128>();
    match possible_int {
        Ok(val) => return Value::some(Value::Int(val)),
        Err(_error) => return Ok(Value::none()),
    }
}

pub fn native_string_to_int(value: Value) -> Result<Value> {
    native_string_to_int_generic(value, safe_convert_string_to_int)
}

fn safe_convert_string_to_uint(raw_string: String) -> Result<Value> {
    let possible_int = raw_string.parse::<u128>();
    match possible_int {
        Ok(val) => return Value::some(Value::UInt(val)),
        Err(_error) => return Ok(Value::none()),
    }
}

pub fn native_string_to_uint(value: Value) -> Result<Value> {
    native_string_to_int_generic(value, safe_convert_string_to_uint)
}

pub fn native_int_to_string_generic(
    value: Value,
    conversion_fn: fn(bytes: Vec<u8>) -> Result<Value>,
) -> Result<Value> {
    match value {
        Value::Int(ref int_value) => {
            let as_string = int_value.to_string();
            match conversion_fn(as_string.into()) {
                Ok(value) => Ok(value),
                Err(_error) => Err(CheckErrors::InternalError.into()),
            }
        }
        Value::UInt(ref uint_value) => {
            let as_string = uint_value.to_string();
            match conversion_fn(as_string.into()) {
                Ok(value) => Ok(value),
                Err(_error) => Err(CheckErrors::InternalError.into()),
            }
        }
        _ => Err(CheckErrors::UnionTypeValueError(
            vec![TypeSignature::IntType, TypeSignature::UIntType],
            value,
        )
        .into()),
    }
}

pub fn native_int_to_ascii(value: Value) -> Result<Value> {
    // Given a string representing an integer, convert this to Clarity ASCII value.
    native_int_to_string_generic(value, Value::string_ascii_from_bytes)
}

pub fn native_int_to_utf8(value: Value) -> Result<Value> {
    // Given a string representing an integer, convert this to Clarity UTF8 value.
    native_int_to_string_generic(value, Value::string_utf8_from_bytes)
}
