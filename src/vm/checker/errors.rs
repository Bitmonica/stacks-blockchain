use vm::representations::SymbolicExpression;
use vm::types::{TypeSignature, AtomTypeIdentifier};
use std::error;
use std::fmt;

pub type CheckResult <T> = Result<T, CheckError>;

#[derive(Debug, PartialEq)]
pub enum CheckErrors {
    // list typing errors
    UnknownListConstructionFailure,
    ListTypesMustMatch,
    ConstructedListTooLarge,

    // simple type expectation mismatch
    TypeError(TypeSignature, TypeSignature),
    // union type mismatch
    UnionTypeError(Vec<TypeSignature>, TypeSignature),
    ExpectedOptionalType,
    ExpectedResponseType,
    CouldNotDetermineResponseOkType,
    CouldNotDetermineResponseErrType,

    // Checker runtime failures
    TypeAlreadyAnnotatedFailure,
    CheckerImplementationFailure,
    TypeNotAnnotatedFailure,

    // tuples
    BadTupleFieldName,
    ExpectedTuple(TypeSignature),
    NoSuchTupleField(String),
    BadTupleConstruction,
    TupleExpectsPairs,

    // data map
    BadMapName,
    NoSuchMap(String),

    // defines
    DefineFunctionBadSignature,
    BadFunctionName,
    BadMapTypeDefinition,
    PublicFunctionMustReturnBool,
    DefineVariableBadSignature,
    ReturnTypesMustMatch,

    // contract-call errors
    NoSuchContract(String),
    NoSuchPublicFunction(String, String),
    ContractAlreadyExists(String),
    ContractCallExpectName,

    // get-block-info errors
    NoSuchBlockInfoProperty(String),
    GetBlockInfoExpectPropertyName,

    NameAlreadyUsed(String),
    // expect a function, or applying a function to a list
    NonFunctionApplication,
    ExpectedListApplication,
    // let syntax
    BadLetSyntax,
    BadSyntaxBinding,
    MaxContextDepthReached,
    UnboundVariable(String),
    VariadicNeedsOneArgument,
    IncorrectArgumentCount(usize, usize),
    IfArmsMustMatch(TypeSignature, TypeSignature),
    DefaultTypesMustMatch(TypeSignature, TypeSignature),
    TooManyExpressions,
    IllegalOrUnknownFunctionApplication(String),
    UnknownFunction(String),

    NotImplemented,
    WriteAttemptedInReadOnly,
}

#[derive(Debug, PartialEq)]
pub struct CheckError {
    pub err: CheckErrors,
    pub expression: Option<SymbolicExpression>
}

impl CheckError {
    pub fn new(err: CheckErrors) -> CheckError {
        CheckError {
            err: err,
            expression: None
        }
    }

    pub fn has_expression(&self) -> bool {
        self.expression.is_some()
    }

    pub fn set_expression(&mut self, expr: &SymbolicExpression) {
        self.expression.replace(expr.clone());
    }
}


impl fmt::Display for CheckError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.err {
            CheckErrors::TypeError(ref t1, ref t2) => {
                if let TypeSignature::Atom(AtomTypeIdentifier::OptionalType(ref inner_type)) = t2 {
                    if t1.admits_type(inner_type) {
                        write!(f, "Type Error: Expected {}, found optional type. You may need to unpack the option value using, e.g., (expects! ...) or (default-to ...).",
                               t1)
                    } else {
                        write!(f, "Type Error: Expected {}, Found {}", t1, t2)
                    }
                } else {
                    write!(f, "Type Error: Expected {}, Found {}", t1, t2)
                }
            },
            _ =>  write!(f, "{:?}", self.err)
        }?;

        if let Some(ref e) = self.expression {
            write!(f, "\nNear:\n{}", e)?;
        }

        Ok(())
    }
}

impl error::Error for CheckError {
    fn source(&self) -> Option<&(dyn error::Error + 'static)> {
        match self.err {
            _ => None
        }
    }
}


#[cfg(test)]
mod tests {
    use vm::checker::typecheck::tests::type_check_program;

    #[test]
    fn test_optional_unwrap() {
        let tests = ["(+ 1 (some 4))",
                     "(+ 2 (some 'true))",
                     "(+ 1 'true)"];
        for test in tests.iter() {
            println!("{}", type_check_program(test).unwrap_err())
        }
        panic!("")
    }
}
