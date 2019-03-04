use vm::types::{Value, TupleTypeSignature, parse_name_type_pairs};
use vm::callables::{DefinedFunction, PublicFunction, PrivateFunction};
use vm::representations::SymbolicExpression;
use vm::representations::SymbolicExpression::{Atom, AtomValue, List, NamedParameter};
use vm::errors::{Error, ErrType, InterpreterResult as Result};
use vm::contexts::{ContractContext, LocalContext, Environment};
use vm::eval;

pub enum DefineResult {
    Variable(String, Value),
    Function(String, DefinedFunction),
    Map(String, TupleTypeSignature, TupleTypeSignature),
    NoDefine
}

fn check_legal_define(name: &str, contract_context: &ContractContext) -> Result<()> {
    use vm::is_reserved;

    if is_reserved(name) {
        Err(Error::new(ErrType::ReservedName(name.to_string())))
    } else if contract_context.variables.contains_key(name) || contract_context.functions.contains_key(name) {
        Err(Error::new(ErrType::VariableDefinedMultipleTimes(name.to_string())))
    } else {
        Ok(())
    }
}

fn handle_define_variable(variable: &String, expression: &SymbolicExpression, env: &mut Environment) -> Result<DefineResult> {
    // is the variable name legal?
    check_legal_define(variable, &env.contract_context)?;
    let context = LocalContext::new();
    let value = eval(expression, env, &context)?;
    Ok(DefineResult::Variable(variable.clone(), value))
}

fn handle_define_private_function(signature: &[SymbolicExpression],
                                  expression: &SymbolicExpression,
                                  env: &Environment) -> Result<DefineResult> {
    let coerced_atoms: Result<Vec<_>> = signature.iter().map(|x| {
        if let Atom(name) = x {
            Ok(name)
        } else {
            Err(Error::new(ErrType::InvalidArguments("Non-atomic argument to method signature in define".to_string())))
        }
    }).collect();

    let names = coerced_atoms?;

    let (function_name, arg_names) = names.split_first()
        .ok_or(Error::new(ErrType::InvalidArguments("Must supply atleast a name argument to define a function".to_string())))?;

    check_legal_define(&function_name, &env.contract_context)?;
    let function = PrivateFunction::new(
        arg_names.iter().map(|x| (*x).clone()).collect(),
        expression.clone(),
        *function_name,
        &"");

    Ok(DefineResult::Function((*function_name).clone(), function))
}

fn handle_define_public_function(signature: &[SymbolicExpression],
                                 expression: &SymbolicExpression,
                                 env: &Environment) -> Result<DefineResult> {
    let (function_symbol, arg_symbols) = signature.split_first()
        .ok_or(Error::new(ErrType::InvalidArguments("Must supply atleast a name argument to define a function".to_string())))?;

    let function_name = match function_symbol {
        Atom(name) => Ok(name),
        _ => Err(Error::new(ErrType::InvalidArguments(format!("Invalid function name {:?}", function_symbol))))
    }?;

    check_legal_define(&function_name, &env.contract_context)?;

    let arguments = parse_name_type_pairs(arg_symbols)?;

    let function = PublicFunction::new(arguments,
                                       expression.clone(),
                                       function_name,
                                       &"");


    Ok(DefineResult::Function(function_name.clone(), function))
}

fn handle_define_map(map_name: &SymbolicExpression,
                     key_type: &SymbolicExpression,
                     value_type: &SymbolicExpression,
                     env: &Environment) -> Result<DefineResult> {
    let map_str = match map_name {
        Atom(ref map_name) => Ok(map_name.clone()),
        _ => Err(Error::new(ErrType::InvalidArguments("Non-name argument to define-map".to_string())))
    }?;

    check_legal_define(&map_str, &env.contract_context)?;

    let key_type_signature = TupleTypeSignature::parse_name_type_pair_list(key_type)?;
    let value_type_signature = TupleTypeSignature::parse_name_type_pair_list(value_type)?;

    Ok(DefineResult::Map(map_str, key_type_signature, value_type_signature))
}

pub fn evaluate_define(expression: &SymbolicExpression, env: &mut Environment) -> Result<DefineResult> {
    if let SymbolicExpression::List(elements) = expression {
        if let Some(Atom(func_name)) = elements.get(0) {
            return match func_name.as_str() {
                "define" => {
                    if elements.len() != 3 {
                        Err(Error::new(ErrType::InvalidArguments("(define ...) requires 2 arguments".to_string())))
                    } else {
                        match elements[1] {
                            Atom(ref variable) => handle_define_variable(variable, &elements[2], env),
                            AtomValue(ref _value) => Err(Error::new(ErrType::InvalidArguments(
                                "Illegal operation: attempted to re-define a value type.".to_string()))),
                            NamedParameter(ref _value) => Err(Error::new(ErrType::InvalidArguments(
                                "Illegal operation: attempted to re-define a named parameter.".to_string()))),
                            List(ref function_signature) =>
                                handle_define_private_function(&function_signature, &elements[2], env)
                        }
                    }
                },
                "define-public" => {
                    if elements.len() != 3 {
                        Err(Error::new(ErrType::InvalidArguments("(define-public ...) requires 2 arguments".to_string())))
                    } else {
                        if let List(ref function_signature) =  elements[1] {
                            handle_define_public_function(&function_signature, &elements[2], env)
                        } else {
                            Err(Error::new(ErrType::InvalidArguments(
                                "Illegal operation: attempted to define-public a non-function.".to_string())))
                        }
                    }
                },
                "define-map" => {
                    if elements.len() != 4 {
                        Err(Error::new(ErrType::InvalidArguments("(define-map ...) requires 3 arguments".to_string())))
                    } else {
                        handle_define_map(&elements[1], &elements[2], &elements[3], env)
                    }
                }
                _ => Ok(DefineResult::NoDefine)
            }
        }
    }

    Ok(DefineResult::NoDefine)
}
