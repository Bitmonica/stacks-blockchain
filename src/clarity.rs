use rand::Rng;
use std::io;
use std::io::{Read, Write};
use std::fs;
use std::env;
use std::process;
use util::log;

use rusqlite::Connection;

use vm::parser::parse;
use vm::contexts::OwnedEnvironment;
use vm::database::{ClarityDatabase, SqliteStore, SqliteConnection, memory_db};
use vm::{SymbolicExpression, SymbolicExpressionType};
use vm::checker::{type_check, AnalysisDatabase};
use vm::checker::typecheck::contexts::ContractAnalysis;
use vm::types::Value;

use address::c32::c32_address;

use serde::Serialize;

#[cfg_attr(tarpaulin, skip)]
fn print_usage(invoked_by: &str) {
    eprintln!("Usage: {} [command]
where command is one of:

  initialize         to initialize a local VM state database.
  mine_block         to simulated mining a new block.
  get_block_height   to print the simulated block height.
  check              to typecheck a potential contract definition.
  launch             to launch a initialize a new contract in the local state database.
  eval               to evaluate (in read-only mode) a program in a given contract context.
  eval_raw           to typecheck and evaluate an expression without a contract or database context.
  repl               to typecheck and evaluate expressions in a stdin/stdout loop.
  execute            to execute a public function of a defined contract.
  generate_address   to generate a random Stacks public address for testing purposes.

", invoked_by);
    process::exit(1);
}

#[cfg_attr(tarpaulin, skip)]
fn friendly_expect<A,B: std::fmt::Display>(input: Result<A,B>, msg: &str) -> A {
    input.unwrap_or_else(|e| {
        eprintln!("{}\nCaused by: {}", msg, e);
        process::exit(1);
    })
}

#[cfg_attr(tarpaulin, skip)]
fn friendly_expect_opt<A>(input: Option<A>, msg: &str) -> A {
    input.unwrap_or_else(|| {
        eprintln!("{}", msg);
        process::exit(1);
    })
}

pub fn invoke_command(invoked_by: &str, args: &[String]) {
    if args.len() < 1 {
        print_usage(invoked_by)
    }

    match args[0].as_ref() {
        "initialize" => {
            if args.len() < 2 {
                eprintln!("Usage: {} {} [vm-state.db]", invoked_by, args[0]);
                process::exit(1);
            }
            match SqliteConnection::initialize(&args[1]) {
                Ok(x) => {
                    let mut db = ClarityDatabase::new(Box::new(x));
                    db.initialize();
                    println!("Database created.");
                }
                Err(error) => {
                    eprintln!("Initialization error: \n{}", error);
                    process::exit(1);
                }
            }
        },
        "generate_address" => {
            // random 20 bytes
            let random_bytes = rand::thread_rng().gen::<[u8; 20]>();
            // version = 22
            let addr = friendly_expect(c32_address(22, &random_bytes), "Failed to generate address");
            println!("{}", addr);
        },
        "mine_block" => {
            // TODO: add optional args for specifying timestamps and number of blocks to mine.
            if args.len() < 3 {
                eprintln!("Usage: {} {} [block time] [vm-state.db]", invoked_by, args[0]);
                process::exit(1);
            }

            let block_time: u64 = friendly_expect(args[1].parse(), "Failed to parse block time");

            let db = match SqliteConnection::open(&args[2]) {
                Ok(db) => db,
                Err(error) => {
                    eprintln!("Could not open vm-state: \n{}", error);
                    process::exit(1);
                }
            };

            let mut db = ClarityDatabase::new(Box::new(db));
            db.begin();
            db.sim_mine_block_with_time(block_time);
            db.commit();
            println!("Simulated block mine!");
        },
        "get_block_height" => {
            if args.len() < 2 {
                eprintln!("Usage: {} {} [vm-state.db]", invoked_by, args[0]);
                process::exit(1);
            }

            let db = match SqliteConnection::open(&args[1]) {
                Ok(db) => db,
                Err(error) => {
                    eprintln!("Could not open vm-state: \n{}", error);
                    process::exit(1);
                }
            };

            let mut db = ClarityDatabase::new(Box::new(db));
            db.begin();
            let blockheight = db.get_simmed_block_height();
            println!("Simulated block height: \n{}", blockheight);
            db.roll_back();
        },
        "check" => {
            if args.len() < 2 {
                eprintln!("Usage: {} {} [program-file.clar] (vm-state.db)", invoked_by, args[0]);
                process::exit(1);
            }
            
            let content: String = friendly_expect(fs::read_to_string(&args[1]),
                                                  &format!("Error reading file: {}", args[1]));
            
            let mut db_conn =
                if args.len() >= 3 {
                    let vm_filename = &args[2];
                    friendly_expect(
                        SqliteConnection::open(vm_filename),
                        &format!("Could not open vm-state: {}", vm_filename))
                } else {
                    friendly_expect(SqliteConnection::memory(), "Could not open in-memory analysis DB")
                };

            let mut db = AnalysisDatabase::new(Box::new(db_conn));

            let mut ast = friendly_expect(parse(&content), "Failed to parse program");
            let contract_analysis = type_check(&":transient:", &mut ast, &mut db, false).unwrap_or_else(|e| {
                println!("{}", &e.diagnostic);
                process::exit(1);
            });

            match args.last() {
                Some(s) if s == "--output_analysis" => {
                    println!("{}", contract_analysis.to_interface().serialize());
                },
                _ => {
                    println!("Checks passed.");
                }
            }
        },
        "repl" => {
            let mut vm_env = OwnedEnvironment::memory();
            let mut exec_env = vm_env.get_exec_environment(None);

            let mut analysis_db = AnalysisDatabase::memory();

            let mut stdout = io::stdout();

            loop {
                let content: String = {
                    let mut buffer = String::new();
                    stdout.write(b"> ").unwrap_or_else(|e| {
                        panic!("Failed to write stdout prompt string:\n{}", e);
                    });
                    stdout.flush().unwrap_or_else(|e| {
                        panic!("Failed to flush stdout prompt string:\n{}", e);
                    });
                    match io::stdin().read_line(&mut buffer) {
                        Ok(_) => buffer,
                        Err(error) => {
                            eprintln!("Error reading from stdin:\n{}", error);
                            process::exit(1);
                        }
                    }
                };

                let mut ast = match parse(&content) {
                    Ok(val) => val,
                    Err(error) => {
                        println!("Parse error:\n{}", error);
                        continue;
                    }
                };

                match type_check(":transient:", &mut ast, &mut analysis_db, true) {
                    Ok(_) => (),
                    Err(error) => {
                        println!("Type check error:\n{}", error);
                        continue;
                    } 
                }

                let eval_result = match exec_env.eval_raw(&content) {
                    Ok(val) => val,
                    Err(error) => {
                        println!("Execution error:\n{}", error);
                        continue;
                    }
                };
                
                println!("{}", eval_result);
            }
        },
        "eval_raw" => {
            let content: String = {
                let mut buffer = String::new();
                friendly_expect(io::stdin().read_to_string(&mut buffer), "Error reading from stdin.");
                buffer
            };

            let mut analysis_db = AnalysisDatabase::memory();

            let mut vm_env = OwnedEnvironment::memory();

            let mut ast = friendly_expect(parse(&content), "Failed to parse program.");
            match type_check(":transient:", &mut ast, &mut analysis_db, true) {
                Ok(_) => {
                    let result = vm_env.get_exec_environment(None).eval_raw(&content);
                    match result {
                        Ok(x) => {
                            println!("Program executed successfully! Output: \n{}", x);
                        },
                        Err(error) => {
                            eprintln!("Program execution error: \n{}", error);
                            process::exit(1);
                        }
                    }
                },
                Err(error) => {
                    eprintln!("Type check error.\n{}", error);
                    process::exit(1);
                }
            }
        },
        "eval" => {
            if args.len() < 3 {
                eprintln!("Usage: {} {} [context-contract-name] (program.clar) [vm-state.db]", invoked_by, args[0]);
                process::exit(1);
            }

            let vm_filename = {
                if args.len() == 3 {
                    &args[2]
                } else {
                    &args[3]
                }
            };
            
            let db = match SqliteConnection::open(vm_filename) {
                Ok(db) => db,
                Err(error) => {
                    eprintln!("Could not open vm-state: \n{}", error);
                    process::exit(1);
                }
            };

            let mut db = ClarityDatabase::new(Box::new(db));

            let content: String = {
                if args.len() == 3 {
                    let mut buffer = String::new();
                    friendly_expect(io::stdin().read_to_string(&mut buffer),
                                    "Error reading from stdin.");
                    buffer
                } else {
                    friendly_expect(fs::read_to_string(&args[2]),
                                    &format!("Error reading file: {}", args[2]))
                }
            };

            let mut vm_env = OwnedEnvironment::new(db);
            let contract_name = &args[1];
            
            let result = vm_env.get_exec_environment(None)
                .eval_read_only(contract_name, &content);

            match result {
                Ok(x) => {
                    println!("Program executed successfully! Output: \n{}", x);
                },
                Err(error) => { 
                    eprintln!("Program execution error: \n{}", error);
                    process::exit(1);
                }
            }
        },
        "launch" => {
            if args.len() < 4 {
                eprintln!("Usage: {} {} [contract-name] [contract-definition.clar] [vm-state.db]", invoked_by, args[0]);
                process::exit(1);
            }
            let vm_filename = &args[3];

            let contract_name = &args[1];
            let contract_content: String = friendly_expect(fs::read_to_string(&args[2]),
                                                           &format!("Error reading file: {}", args[2]));

            // typecheck and insert into typecheck tables
            // Aaron todo: AnalysisDatabase and ContractDatabase now use savepoints
            //     on the same connection, so they can abort together, _however_,
            //     this results in some pretty weird function interfaces. I'll need
            //     to think about whether or not there's a more ergonomic way to do this.


            let mut db_conn = match SqliteConnection::open(vm_filename) {
                Ok(db) => db,
                Err(error) => {
                    eprintln!("Could not open vm-state: \n{}", error);
                    process::exit(1);
                }
            };

            let mut outer_sp = db_conn.begin_save_point_raw();
            let contract_analysis: ContractAnalysis;

            { 
                let sql_data_store = SqliteStore::new(&outer_sp);
                let mut db = AnalysisDatabase::new(Box::new(sql_data_store));

                let mut ast = friendly_expect(parse(&contract_content), "Failed to parse program.");

                contract_analysis = friendly_expect(type_check(contract_name, &mut ast, &mut db, true),
                                                    "Type check error.");
            }

            let result = {
                let sql_data_store = SqliteStore::new(&outer_sp);
                let db = ClarityDatabase::new(Box::new(sql_data_store));
                let mut vm_env = OwnedEnvironment::new(db);
                vm_env.initialize_contract(&contract_name, &contract_content)
            };

            match result {
                Ok(_x) => {
                    friendly_expect(outer_sp.commit(), "Failed to commit results to database");
                    match args.last() {
                        Some(s) if s == "--output_analysis" => {
                            println!("{}", contract_analysis.to_interface().serialize());
                        },
                        _ => {
                            println!("Contract initialized!");
                        }
                    }
                },
                Err(error) => {
                    eprintln!("Contract initialization error: \n{}", error);
                    process::exit(1);
                }
            }
        },
        "execute" => {
            if args.len() < 5 {
                eprintln!("Usage: {} {} [vm-state.db] [contract-name] [public-function-name] [sender-address] [args...]", invoked_by, args[0]);
                process::exit(1);
            }
            let vm_filename = &args[1];

            let db = match SqliteConnection::open(vm_filename) {
                Ok(db) => ClarityDatabase::new(Box::new(db)),
                Err(error) => {
                    eprintln!("Could not open vm-state: \n{}", error);
                    process::exit(1);
                }
            };

            let mut vm_env = OwnedEnvironment::new(db);
            let contract_name = &args[2];
            let tx_name = &args[3];
            
            let sender_in = &args[4];

            let mut sender = 
                friendly_expect_opt(
                    friendly_expect(parse(&format!("'{}", sender_in)),
                                    &format!("Error parsing sender {}", sender_in))
                        .pop(),
                    &format!("Failed to read a sender from {}", sender_in));
            let sender = {
                if let Some(Value::Principal(principal_data)) = sender.match_atom_value() {
                    Value::Principal(principal_data.clone())
                } else {
                    eprintln!("Unexpected result parsing sender: {}", sender_in);
                    process::exit(1);
                }
            };
            let arguments: Vec<_> = args[5..]
                .iter()
                .map(|argument| {
                    let mut argument_parsed = friendly_expect(
                        parse(argument),
                        &format!("Error parsing argument \"{}\"", argument));
                    let argument_value = friendly_expect_opt(
                        argument_parsed.pop(),
                        &format!("Failed to parse a value from the argument: {}", argument));
                    let argument_value = friendly_expect_opt(
                        argument_value.match_atom_value(),
                        &format!("Expected a literal value from the argument: {}", argument));
                    SymbolicExpression::atom_value(argument_value.clone())
                })
                .collect();

            let result = vm_env.execute_transaction(sender, &contract_name, &tx_name, &arguments);

            match result {
                Ok((x, _)) => {
                    if let Value::Response(data) = x {
                        if data.committed {
                            println!("Transaction executed and committed. Returned: {}", data.data);
                        } else {
                            println!("Aborted: {}", data.data);
                        }
                    } else {
                        panic!(format!("Expected a ResponseType result from transaction. Found: {}", x));
                    }
                },
                Err(error) => {
                    eprintln!("Transaction execution error: \n{}", error);
                    process::exit(1);
                }
            }
        },
        _ => {
            print_usage(invoked_by)
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    #[test]
    fn test_samples() {
        let db_name = format!("/tmp/db_{}", rand::thread_rng().gen::<i32>());
        invoke_command("test", &["initialize".to_string(), db_name.clone()]);
        invoke_command("test", &["check".to_string(), "sample-programs/tokens.clar".to_string(), db_name.clone()]);
        invoke_command("test", &["launch".to_string(), "tokens".to_string(),
                                 "sample-programs/tokens.clar".to_string(), db_name.clone()]);
        invoke_command("test", &["execute".to_string(), db_name.clone(), "tokens".to_string(),
                                 "mint!".to_string(), "SZ2J6ZY48GV1EZ5V2V5RB9MP66SW86PYKKQ9H6DPR".to_string(),
                                 "1000".to_string()]);
    }
}
