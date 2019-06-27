use std::fmt;
use vm::types::{Value};

/*
 I don't add a pair type here, since we're only using these S-Expressions to represent code, rather than
 data structures, and we don't support pair expressions directly in our lisp dialect.
 */

#[derive(Debug)]
#[derive(Serialize, Deserialize)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub enum SymbolicExpressionType {
    AtomValue(Value),
    Atom(String),
    List(Box<[SymbolicExpression]>),
}

#[derive(Debug)]
#[derive(Serialize, Deserialize)]
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SymbolicExpression {
    pub expr: SymbolicExpressionType,
    // this id field is used by compiler passes to store information in
    //  maps.
    // first pass       -> fill out unique ids
    // ...typing passes -> store information in hashmap according to id.
    // 
    // this is a fairly standard technique in compiler passes
    pub id: u64,

    #[cfg(feature = "developer-mode")]
    pub line_number: u32,
}

impl SymbolicExpression {
    #[cfg(feature = "developer-mode")]
    fn cons() -> Self {
        Self {
            id: 0,
            line_number: 0,
            expr: SymbolicExpressionType::AtomValue(Value::Bool(false))
        }
    }
    #[cfg(not(feature = "developer-mode"))]
    fn cons() -> Self {
        Self {
            id: 0,
            expr: SymbolicExpressionType::AtomValue(Value::Bool(false))
        }
    }

    #[cfg(feature = "developer-mode")]
    pub fn set_line_number(&mut self, line_number: u32) {
        self.line_number = line_number
    }

    #[cfg(not(feature = "developer-mode"))]
    pub fn set_line_number(&mut self, _line_number: u32) {
    }

    pub fn atom_value(val: Value) -> SymbolicExpression {
        SymbolicExpression {
            expr: SymbolicExpressionType::AtomValue(val),
            .. SymbolicExpression::cons()
        }
    }

    pub fn atom(val: String) -> SymbolicExpression {
        SymbolicExpression {
            expr: SymbolicExpressionType::Atom(val),
            .. SymbolicExpression::cons()
        }
    }

    pub fn list(val: Box<[SymbolicExpression]>) -> SymbolicExpression {
        SymbolicExpression {
            expr: SymbolicExpressionType::List(val),
            .. SymbolicExpression::cons()
        }
    }

    // These match functions are used to simplify calling code
    //   areas a lot. There is a frequent code pattern where
    //   a block _expects_ specific symbolic expressions, leading
    //   to a lot of very verbose `if let x = {` expressions. 

    pub fn match_list(&self) -> Option<&[SymbolicExpression]> {
        if let SymbolicExpressionType::List(ref list) = self.expr {
            Some(list)
        } else {
            None
        }
    }

    pub fn match_atom(&self) -> Option<&String> {
        if let SymbolicExpressionType::Atom(ref value) = self.expr {
            Some(value)
        } else {
            None
        }
    }

    pub fn match_atom_value(&self) -> Option<&Value> {
        if let SymbolicExpressionType::AtomValue(ref value) = self.expr {
            Some(value)
        } else {
            None
        }
    }
}

impl fmt::Display for SymbolicExpression {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self.expr {
            SymbolicExpressionType::List(ref list) => {
                write!(f, "(")?;
                let mut list_prefix = "";
                for item in list.iter() {
                    write!(f, "{}", list_prefix)?;
                    write!(f, "{}", item)?;
                    list_prefix = " ";
                }
                write!(f, ")")?;
            },
            SymbolicExpressionType::Atom(ref value) => {
                write!(f, "{}", value)?;
            },
            SymbolicExpressionType::AtomValue(ref value) => {
                write!(f, "{}", value)?;
            }
        };
        
        Ok(())
    }
}
