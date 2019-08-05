use std::convert::TryFrom;

use rusqlite::{Connection, OptionalExtension, NO_PARAMS, Row, Savepoint};
use rusqlite::types::{ToSql, FromSql};

use vm::contracts::Contract;
use vm::errors::{Error, InterpreterError, RuntimeErrorType, UncheckedError, InterpreterResult as Result, IncomparableError};
use vm::types::{Value, OptionalData, TypeSignature, TupleTypeSignature, AtomTypeIdentifier, PrincipalData, NONE};

use chainstate::burn::{VRFSeed, BlockHeaderHash};
use burnchains::BurnchainHeaderHash;

const SQL_FAIL_MESSAGE: &str = "PANIC: SQL Failure in Smart Contract VM.";
const DESERIALIZE_FAIL_MESSAGE: &str = "PANIC: Failed to deserialize bad database data in Smart Contract VM.";
const SIMMED_BLOCK_TIME: u64 = 10 * 60; // 10 min

enum DataType {
    DATA_MAP = 0,
    VARIABLE = 1,
    TOKEN    = 2,
    ASSET    = 3,
    TOKEN_SUPPLY = 4,
}

const DUMMY_KEY: &str = "";

pub struct ContractDatabaseConnection {
    conn: Connection
}

pub struct ContractDatabase <'a> {
    savepoint: Savepoint<'a>
}

pub struct SqliteToken {
    token_identifier: i64,
    total_supply: Option<i128>
}

pub struct SqliteAsset {
    asset_identifier: i64,
    key_type: TypeSignature
}

pub struct SqliteDataMap {
    map_identifier: i64,
    key_type: TypeSignature,
    value_type: TypeSignature
}

pub struct SqliteDataVariable {
    variable_identifier: i64,
    value_type: TypeSignature
}

pub trait ContractDatabaseTransacter {
    fn begin_save_point(&mut self) -> ContractDatabase<'_>;
}

impl ContractDatabaseConnection {
    pub fn initialize(filename: &str) -> Result<ContractDatabaseConnection> {
        let mut contract_db = ContractDatabaseConnection::inner_open(filename)?;
        contract_db.execute("CREATE TABLE IF NOT EXISTS maps_table
                      (map_identifier INTEGER PRIMARY KEY AUTOINCREMENT,
                       contract_name TEXT NOT NULL,
                       map_name TEXT NOT NULL,
                       key_type TEXT NOT NULL,
                       value_type TEXT NOT NULL,
                       UNIQUE(contract_name, map_name))",
                            NO_PARAMS);
        contract_db.execute("CREATE TABLE IF NOT EXISTS variables_table
                      (variable_identifier INTEGER PRIMARY KEY AUTOINCREMENT,
                       contract_name TEXT NOT NULL,
                       variable_name TEXT NOT NULL,
                       value_type TEXT NOT NULL,
                       UNIQUE(contract_name, variable_name))",
                            NO_PARAMS);
        contract_db.execute("CREATE TABLE IF NOT EXISTS assets_table
                      (asset_identifier INTEGER PRIMARY KEY AUTOINCREMENT,
                       contract_name TEXT NOT NULL,
                       asset_name TEXT NOT NULL,
                       key_type TEXT NOT NULL,
                       UNIQUE(contract_name, asset_name))",
                            NO_PARAMS);
        contract_db.execute("CREATE TABLE IF NOT EXISTS tokens_table
                      (token_identifier INTEGER PRIMARY KEY AUTOINCREMENT,
                       contract_name TEXT NOT NULL,
                       token_name TEXT NOT NULL,
                       total_supply TEXT,
                       UNIQUE(contract_name, token_name))",
                            NO_PARAMS);
        contract_db.execute("CREATE TABLE IF NOT EXISTS data_table
                      (data_identifier INTEGER PRIMARY KEY AUTOINCREMENT,
                       data_type INTEGER NOT NULL,
                       data_store_identifier INTEGER NOT NULL,
                       key TEXT NOT NULL,
                       value TEXT)",
                            NO_PARAMS);
        contract_db.execute("CREATE TABLE IF NOT EXISTS contracts
                      (contract_identifier INTEGER PRIMARY KEY AUTOINCREMENT,
                       contract_name TEXT UNIQUE NOT NULL,
                       contract_data TEXT NOT NULL)",
                            NO_PARAMS);

        contract_db.execute("CREATE TABLE IF NOT EXISTS simmed_block_table
                      (block_height INTEGER PRIMARY KEY,
                       block_time INTEGER NOT NULL,
                       block_vrf_seed BLOB NOT NULL,
                       block_header_hash BLOB NOT NULL,
                       burnchain_block_header_hash BLOB NOT NULL)",
                            NO_PARAMS);
        
        // Insert 20 simulated blocks
        // TODO: Only perform this when in a local dev environment.
        let simmed_default_height: u64 = 0;
        let simmed_block_count: u64 = 20;
        use std::time::{SystemTime, UNIX_EPOCH};
        let time_now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Time went backwards")
            .as_secs();

        for i in simmed_default_height..simmed_block_count {
            let block_time = i64::try_from(time_now - ((simmed_block_count - i) * SIMMED_BLOCK_TIME)).unwrap();
            let block_height = i64::try_from(i).unwrap();

            let mut block_vrf = [0u8; 32];
            block_vrf[0] = 1;
            block_vrf[31] = i as u8;
            let block_vrf = VRFSeed::from_bytes(&block_vrf).unwrap();

            let mut header_hash = vec![0u8; 32];
            header_hash[0] = 2;
            header_hash[31] = block_height as u8;
            let header_hash = BlockHeaderHash::from_bytes(&header_hash).unwrap();

            let mut burnchain_header_hash = vec![0u8; 32];
            burnchain_header_hash[0] = 3;
            burnchain_header_hash[31] = block_height as u8;
            let burnchain_header_hash = BurnchainHeaderHash::from_bytes(&burnchain_header_hash).unwrap();

            contract_db.execute("INSERT INTO simmed_block_table 
                            (block_height, block_time, block_vrf_seed, block_header_hash, burnchain_block_header_hash) 
                            VALUES (?1, ?2, ?3, ?4, ?5)",
                            &[&block_height as &ToSql, &block_time,
                            &block_vrf.to_bytes().to_vec(),
                            &header_hash.to_bytes().to_vec(),
                            &burnchain_header_hash.to_bytes().to_vec()]);
        }

        contract_db.check_schema()?;

        Ok(contract_db)
    }

    pub fn memory() -> Result<ContractDatabaseConnection> {
        ContractDatabaseConnection::initialize(":memory:")
    }

    pub fn open(filename: &str) -> Result<ContractDatabaseConnection> {
        let contract_db = ContractDatabaseConnection::inner_open(filename)?;

        contract_db.check_schema()?;
        Ok(contract_db)
    }

    pub fn check_schema(&self) -> Result<()> {
        let sql = "SELECT sql FROM sqlite_master WHERE name=?";
        let _: String = self.conn.query_row(sql, &["maps_table"],
                                            |row| row.get(0))
            .map_err(|x| InterpreterError::SqliteError(IncomparableError{ err: x }))?;
        let _: String = self.conn.query_row(sql, &["contracts"],
                                            |row| row.get(0))
            .map_err(|x| InterpreterError::SqliteError(IncomparableError{ err: x }))?;
        let _: String = self.conn.query_row(sql, &["data_table"],
                                            |row| row.get(0))
            .map_err(|x| InterpreterError::SqliteError(IncomparableError{ err: x }))?;
        let _: String = self.conn.query_row(sql, &["simmed_block_table"],
                                            |row| row.get(0))
            .map_err(|x| InterpreterError::SqliteError(IncomparableError{ err: x }))?;
        Ok(())
    }

    pub fn inner_open(filename: &str) -> Result<ContractDatabaseConnection> {
        let conn = Connection::open(filename)
            .map_err(|x| InterpreterError::SqliteError(IncomparableError{ err: x }))?;
        Ok(ContractDatabaseConnection {
            conn: conn
        })
    }

    pub fn execute<P>(&mut self, sql: &str, params: P) -> usize
    where
        P: IntoIterator,
        P::Item: ToSql {
        self.conn.execute(sql, params)
            .expect(SQL_FAIL_MESSAGE)
    }

    pub fn begin_save_point_raw(&mut self) -> Savepoint<'_> {
        self.conn.savepoint()
            .expect(SQL_FAIL_MESSAGE)
    }
}

impl ContractDatabaseTransacter for ContractDatabaseConnection {
    fn begin_save_point(&mut self) -> ContractDatabase<'_> {
        let sp = self.conn.savepoint()
            .expect(SQL_FAIL_MESSAGE);
        ContractDatabase::from_savepoint(sp)
    }
}

impl <'a> ContractDatabase <'a> {
    pub fn from_savepoint(sp: Savepoint<'a>) -> ContractDatabase<'a> {
        ContractDatabase {
            savepoint: sp }
    }

    pub fn execute<P>(&mut self, sql: &str, params: P) -> usize
    where
        P: IntoIterator,
        P::Item: ToSql {
        self.savepoint.execute(sql, params)
            .expect(SQL_FAIL_MESSAGE)
    }

    fn query_row<T, P, F>(&self, sql: &str, params: P, f: F) -> Option<T>
    where
        P: IntoIterator,
        P::Item: ToSql,
        F: FnOnce(&Row) -> T {
        self.savepoint.query_row(sql, params, f)
            .optional()
            .expect(SQL_FAIL_MESSAGE)
    }

    fn key_value_lookup<T>(&self, data_type: DataType, data_store_identifier: &ToSql, key: &ToSql) -> Option<T>
        where T: FromSql {
        let params: [&ToSql; 3] = [&(data_type as u8),
                                   data_store_identifier,
                                   key];
        self.query_row(
            "SELECT value FROM data_table WHERE data_type = ? AND data_store_identifier = ? AND key = ? ORDER BY data_identifier DESC LIMIT 1",
            &params,
            |row| row.get(0))
    }

    fn load_map(&self, contract_name: &str, map_name: &str) -> Result<SqliteDataMap> {
        let (map_identifier, key_type, value_type): (_, String, String) =
            self.query_row(
                "SELECT map_identifier, key_type, value_type FROM maps_table WHERE contract_name = ? AND map_name = ?",
                &[contract_name, map_name],
                |row| {
                    (row.get(0), row.get(1), row.get(2))
                })
            .ok_or(UncheckedError::UndefinedMap(map_name.to_string()))?;

        Ok(SqliteDataMap {
            map_identifier: map_identifier,
            key_type: TypeSignature::deserialize(&key_type),
            value_type: TypeSignature::deserialize(&value_type)
        })
    }

    fn load_contract(&self, contract_name: &str) -> Option<Contract> {
        let contract: Option<String> =
            self.query_row(
                "SELECT contract_data FROM contracts WHERE contract_name = ?",
                &[contract_name],
                |row| {
                    row.get(0)
                });
        match contract {
            None => None,
            Some(ref contract) => Some(
                Contract::deserialize(contract))
        }
    }

    fn load_variable(&self, contract_name: &str, variable_name: &str) -> Result<SqliteDataVariable> {
        let (variable_identifier, value_type): (_, String) =
            self.query_row(
                "SELECT variable_identifier, value_type FROM variables_table WHERE contract_name = ? AND variable_name = ?",
                &[contract_name, variable_name],
                |row| {
                    (row.get(0), row.get(1))
                })
            .ok_or(UncheckedError::UndefinedVariable(variable_name.to_string()))?;

        Ok(SqliteDataVariable {
            variable_identifier: variable_identifier,
            value_type: TypeSignature::deserialize(&value_type)
        })
    }

    pub fn create_variable(&mut self, contract_name: &str, variable_name: &str, value_type: TypeSignature) {
        self.execute("INSERT INTO variables_table (contract_name, variable_name, value_type) VALUES (?, ?, ?)",
                     &[contract_name, variable_name, &value_type.serialize()]);
    }

    pub fn set_variable(&mut self, contract_name: &str, variable_name: &str, value: Value) -> Result<Value> {
        let variable_descriptor = self.load_variable(contract_name, variable_name)?;
        if !variable_descriptor.value_type.admits(&value) {
            return Err(UncheckedError::TypeError(format!("{:?}", variable_descriptor.value_type), value).into())
        }

        let params: [&ToSql; 4] = [&(DataType::VARIABLE as u8),
                                   &variable_descriptor.variable_identifier,
                                   &value.serialize(),
                                   &DUMMY_KEY];

        self.execute(
            "INSERT INTO data_table (data_type, data_store_identifier, value, key) VALUES (?, ?, ?, ?)",
            &params);

        return Ok(Value::Bool(true))
    }

    pub fn lookup_variable(&self, contract_name: &str, variable_name: &str) -> Result<Option<Value>>  {
        let variable_descriptor = self.load_variable(contract_name, variable_name)?;

        let sql_result: Option<Option<String>> =
            self.key_value_lookup(DataType::VARIABLE, &variable_descriptor.variable_identifier, &DUMMY_KEY);
        match sql_result {
            None => Ok(None),
            Some(sql_result) => {
                match sql_result {
                    None => Ok(None),
                    Some(value_data) => Ok(Some(Value::deserialize(&value_data)))
                }
            }
        }
    }

    fn load_token(&self, contract_name: &str, token_name: &str) -> Result<SqliteToken> {
        let (identifier, total_supply) =
            self.query_row(
                "SELECT token_identifier, total_supply FROM tokens_table WHERE contract_name = ? AND token_name = ?",
                &[contract_name, token_name],
                |row| (row.get(0), row.get(1)))
            .ok_or(UncheckedError::UndefinedAssetType(token_name.to_string()))?;

        Ok(SqliteToken {
            token_identifier: identifier,
            total_supply })
    }

    fn load_asset(&self, contract_name: &str, asset_name: &str) -> Result<SqliteAsset> {
        let (identifier, key_type) : (_, String) =
            self.query_row(
                "SELECT asset_identifier, key_type FROM assets_table WHERE contract_name = ? AND asset_name = ?",
                &[contract_name, asset_name],
                |row| (row.get(0), row.get(1)))
            .ok_or(UncheckedError::UndefinedAssetType(asset_name.to_string()))?;

        Ok(SqliteAsset {
            asset_identifier: identifier,
            key_type: TypeSignature::deserialize(&key_type) })
    }

    pub fn create_token(&mut self, contract_name: &str, token_name: &str, total_supply: &Option<i128>) {
        let contract_name_owned = contract_name.to_string();
        let contract_name_owned = token_name.to_string();
        
        let params: [&ToSql; 3] = [&contract_name, &token_name, total_supply];
        self.execute("INSERT INTO tokens_table (contract_name, token_name, total_supply) VALUES (?, ?, ?)",
                     &params);
        if total_supply.is_some() {
            let descriptor = self.load_token(contract_name, token_name)
                .expect("ERROR: VM failed to initialize token correctly.");

            let params: [&ToSql; 4] = [&(DataType::TOKEN_SUPPLY as u8),
                                       &descriptor.token_identifier,
                                       &DUMMY_KEY,
                                       &(0 as i128)];

            self.execute(
                "INSERT INTO data_table (data_type, data_store_identifier, key, value) VALUES (?, ?, ?, ?)",
                &params);
        }
    }

    pub fn create_asset(&mut self, contract_name: &str, asset_name: &str, key_type: &TypeSignature) {
        self.execute("INSERT INTO assets_table (contract_name, asset_name, key_type) VALUES (?, ?, ?)",
                     &[contract_name, asset_name, &key_type.serialize()]);
    }

    pub fn checked_increase_token_supply(&mut self, contract_name: &str, token_name: &str, amount: i128) -> Result<()> {
        if amount < 0 {
            panic!("ERROR: Clarity VM attempted to increase token supply by negative balance.");
        }
        let descriptor = self.load_token(contract_name, token_name)?;

        if let Some(total_supply) = descriptor.total_supply {
            let current_supply_opt: Option<i128> =
                self.key_value_lookup(DataType::TOKEN_SUPPLY, &descriptor.token_identifier, &DUMMY_KEY);
            let current_supply = current_supply_opt
                .expect("ERROR: Clarity VM failed to track token supply.");
            let new_supply = current_supply.checked_add(amount)
                .ok_or(RuntimeErrorType::ArithmeticOverflow)?;

            if new_supply > total_supply {
                Err(RuntimeErrorType::SupplyOverflow(new_supply, total_supply).into())
            } else {
                let params: [&ToSql; 4] = [&(DataType::TOKEN_SUPPLY as u8),
                                           &descriptor.token_identifier,
                                           &DUMMY_KEY,
                                           &new_supply];
                self.execute(
                    "INSERT INTO data_table (data_type, data_store_identifier, key, value) VALUES (?, ?, ?, ?)",
                    &params);
                Ok(())
            }
        } else {
            Ok(())
        }
    }

    // Asset functions return error if no such token was defined by `define-token`
    pub fn get_token_balance(&mut self, contract_name: &str, token_name: &str, principal: &PrincipalData) -> Result<i128> {
        let descriptor = self.load_token(contract_name, token_name)?;

        let params: [&ToSql; 3] = [&(DataType::TOKEN as u8),
                                   &descriptor.token_identifier,
                                   &principal.serialize()];
        let sql_result: Option<i128> = 
            self.query_row(
                "SELECT value FROM data_table WHERE data_type = ? AND data_store_identifier = ? AND key = ? ORDER BY data_identifier DESC LIMIT 1",
                &params,
                |row| row.get(0));
        match sql_result {
            None => Ok(0),
            Some(balance) => Ok(balance)
        }
    }

    pub fn set_token_balance(&mut self, contract_name: &str, token_name: &str, principal: &PrincipalData, balance: i128) -> Result<()> {
        if balance < 0 {
            panic!("ERROR: Clarity VM attempted to set negative token balance.");
        }
        let descriptor = self.load_token(contract_name, token_name)?;

        let params: [&ToSql; 4] = [&(DataType::TOKEN as u8),
                                   &descriptor.token_identifier,
                                   &principal.serialize(),
                                   &balance];
        self.execute(
            "INSERT INTO data_table (data_type, data_store_identifier, key, value) VALUES (?, ?, ?, ?)",
            &params);

        Ok(())
    }

    pub fn get_asset_key_type(&mut self, contract_name: &str, asset_name: &str) -> Result<TypeSignature> {
        let descriptor = self.load_asset(contract_name, asset_name)?;
        Ok(descriptor.key_type)
    }

    // Return errors if no such asset name, or asset is not the correct type.
    pub fn set_asset_owner(&mut self, contract_name: &str, asset_name: &str, asset: &Value, principal: &PrincipalData) -> Result<()> {
        let descriptor = self.load_asset(contract_name, asset_name)?;
        if !descriptor.key_type.admits(asset) {
            return Err(UncheckedError::TypeError(descriptor.key_type.to_string(), (*asset).clone()).into())
        }

        let params: [&ToSql; 4] = [&(DataType::ASSET as u8),
                                   &descriptor.asset_identifier,
                                   &asset.serialize(),
                                   &principal.serialize()];
        self.execute(
            "INSERT INTO data_table (data_type, data_store_identifier, key, value) VALUES (?, ?, ?, ?)",
            &params);

        Ok(())
    }

    pub fn get_asset_owner(&mut self, contract_name: &str, asset_name: &str, asset: &Value) -> Result<PrincipalData> {
        let descriptor = self.load_asset(contract_name, asset_name)?;
        if !descriptor.key_type.admits(asset) {
            return Err(UncheckedError::TypeError(descriptor.key_type.to_string(), (*asset).clone()).into())
        }

        let params: [&ToSql; 3] = [&(DataType::ASSET as u8),
                                   &descriptor.asset_identifier,
                                   &asset.serialize()];
        let sql_result: Option<Option<String>> = 
            self.query_row(
                "SELECT value FROM data_table WHERE data_type = ? AND data_store_identifier = ? AND key = ? ORDER BY data_identifier DESC LIMIT 1",
                &params,
                |row| {
                    row.get(0)
                });
        let deserialized = match sql_result {
            None => None,
            Some(sql_result) => {
                match sql_result {
                    None => None,
                    Some(value_data) => Some(PrincipalData::deserialize(&value_data))
                }
            }
        };

        deserialized.ok_or(RuntimeErrorType::NoSuchAsset.into())
    }

    pub fn create_map(&mut self, contract_name: &str, map_name: &str, key_type: TupleTypeSignature, value_type: TupleTypeSignature) {
        let key_type = TypeSignature::new_atom(AtomTypeIdentifier::TupleType(key_type));
        let value_type = TypeSignature::new_atom(AtomTypeIdentifier::TupleType(value_type));

        self.execute("INSERT INTO maps_table (contract_name, map_name, key_type, value_type) VALUES (?, ?, ?, ?)",
                     &[contract_name, map_name, &key_type.serialize(), &value_type.serialize()]);
    }

    pub fn fetch_entry(&self, contract_name: &str, map_name: &str, key: &Value) -> Result<Option<Value>> {
        let map_descriptor = self.load_map(contract_name, map_name)?;
        if !map_descriptor.key_type.admits(key) {
            return Err(UncheckedError::TypeError(format!("{:?}", map_descriptor.key_type), (*key).clone()).into())
        }

        let params: [&ToSql; 3] = [&(DataType::DATA_MAP as u8),
                                   &map_descriptor.map_identifier,
                                   &key.serialize()];

        let sql_result: Option<Option<String>> = 
            self.query_row(
                "SELECT value FROM data_table WHERE data_type = ? AND data_store_identifier = ? AND key = ? ORDER BY data_identifier DESC LIMIT 1",
                &params,
                |row| {
                    row.get(0)
                });
        match sql_result {
            None => Ok(None),
            Some(sql_result) => {
                match sql_result {
                    None => Ok(None),
                    Some(value_data) => Ok(Some(Value::deserialize(&value_data)))
                }
            }
        }
    }

    pub fn set_entry(&mut self, contract_name: &str, map_name: &str, key: Value, value: Value) -> Result<Value> {
        self.inner_set_entry(contract_name, map_name, key, value, false)
    }

    pub fn insert_entry(&mut self, contract_name: &str, map_name: &str, key: Value, value: Value) -> Result<Value> {
        self.inner_set_entry(contract_name, map_name, key, value, true)
    }
    
    fn inner_set_entry(&mut self, contract_name: &str, map_name: &str, key: Value, value: Value, return_if_exists: bool) -> Result<Value> {
        let map_descriptor = self.load_map(contract_name, map_name)?;
        if !map_descriptor.key_type.admits(&key) {
            return Err(UncheckedError::TypeError(format!("{:?}", map_descriptor.key_type), key).into())
        }
        if !map_descriptor.value_type.admits(&value) {
            return Err(UncheckedError::TypeError(format!("{:?}", map_descriptor.value_type), value).into())
        }

        if return_if_exists && self.fetch_entry(contract_name, map_name, &key)?.is_some() {
            return Ok(Value::Bool(false))
        }

        let params: [&ToSql; 4] = [&(DataType::DATA_MAP as u8),
                                   &map_descriptor.map_identifier,
                                   &key.serialize(),
                                   &Some(value.serialize())];

        self.execute(
            "INSERT INTO data_table (data_type, data_store_identifier, key, value) VALUES (?, ?, ?, ?)",
            &params);

        return Ok(Value::Bool(true))
    }

    pub fn delete_entry(&mut self, contract_name: &str, map_name: &str, key: &Value) -> Result<Value> {
        let exists = self.fetch_entry(contract_name, map_name, &key)?.is_some();
        if !exists {
            return Ok(Value::Bool(false))
        }

        let map_descriptor = self.load_map(contract_name, map_name)?;
        if !map_descriptor.key_type.admits(key) {
            return Err(UncheckedError::TypeError(format!("{:?}", map_descriptor.key_type), (*key).clone()).into())
        }

        let none: Option<String> = None;
        let params: [&ToSql; 4] = [&(DataType::DATA_MAP as u8),
                                   &map_descriptor.map_identifier,
                                   &key.serialize(),
                                   &none];

        self.execute(
            "INSERT INTO data_table (data_type, data_store_identifier, key, value) VALUES (?, ?, ?, ?)",
            &params);

        return Ok(Value::Bool(exists))
    }


    pub fn get_contract(&mut self, contract_name: &str) -> Result<Contract> {
        self.load_contract(contract_name)
            .ok_or_else(|| { UncheckedError::UndefinedContract(contract_name.to_string()).into() })
    }

    pub fn insert_contract(&mut self, contract_name: &str, contract: Contract) {
        if self.load_contract(contract_name).is_some() {
            panic!("Contract already exists {}", contract_name);
        } else {
            self.execute("INSERT INTO contracts (contract_name, contract_data) VALUES (?, ?)",
                         &[contract_name, &contract.serialize()]);
        }
    }

    pub fn get_simmed_block_height(&self) -> Result<u64> {
        let block_height: (i64) =
            self.query_row(
                "SELECT block_height FROM simmed_block_table ORDER BY block_height DESC LIMIT 1",
                NO_PARAMS,
                |row| row.get(0))
            .expect("Failed to fetch simulated block height");

        u64::try_from(block_height)
            .map_err(|_| RuntimeErrorType::Arithmetic("Overflowed fetching block height".to_string()).into())
    }

    pub fn get_simmed_block_time(&self, block_height: u64) -> Result<u64> {
        let block_height = i64::try_from(block_height).unwrap();
        let block_time: (i64) = 
            self.query_row(
                "SELECT block_time FROM simmed_block_table WHERE block_height = ? LIMIT 1",
                &[block_height],
                |row| row.get(0))
            .expect("Failed to fetch simulated block time");

        u64::try_from(block_time)
            .map_err(|_| RuntimeErrorType::Arithmetic("Overflowed fetching block time".to_string()).into())
    }

    pub fn get_simmed_block_header_hash(&self, block_height: u64) -> Result<BlockHeaderHash> {
        let block_height = i64::try_from(block_height).unwrap();
        let block_header_hash: (Vec<u8>) =
            self.query_row(
                "SELECT block_header_hash from simmed_block_table WHERE block_height = ? LIMIT 1",
                &[block_height],
                |row| row.get(0))
            .expect("Failed to fetch simulated block header hash");
        
        BlockHeaderHash::from_bytes(&block_header_hash)
            .ok_or(RuntimeErrorType::ParseError("Failed to instantiate BlockHeaderHash from simmed db data".to_string()).into())
    }

    pub fn get_simmed_burnchain_block_header_hash(&self, block_height: u64) -> Result<BurnchainHeaderHash> {
        let block_height = i64::try_from(block_height).unwrap();
        let block_header_hash: (Vec<u8>) =
            self.query_row(
                "SELECT burnchain_block_header_hash from simmed_block_table WHERE block_height = ? LIMIT 1",
                &[block_height],
                |row| row.get(0))
            .expect("Failed to fetch simulated block header hash");
        
        BurnchainHeaderHash::from_bytes(&block_header_hash)
            .ok_or(RuntimeErrorType::ParseError("Failed to instantiate BurnchainHeaderHash from simmed db data".to_string()).into())
    }

    pub fn get_simmed_block_vrf_seed(&self, block_height: u64) -> Result<VRFSeed> {
        let block_height = i64::try_from(block_height).unwrap();
        let block_vrf_seed: (Vec<u8>) =
            self.query_row(
                "SELECT block_vrf_seed from simmed_block_table WHERE block_height = ? LIMIT 1",
                &[block_height],
                |row| row.get(0))
            .expect("Failed to fetch simulated block vrf seed");
        VRFSeed::from_bytes(&block_vrf_seed)
            .ok_or(RuntimeErrorType::ParseError("Failed to instantiate VRF seed from simmed db data".to_string()).into())
    }

    pub fn sim_mine_block_with_time(&mut self, block_time: u64) {
        let current_height = self.get_simmed_block_height()
            .expect("Failed to get simulated block height");

        let block_height = current_height + 1;
        let block_height = i64::try_from(block_height).unwrap();

        let block_time = i64::try_from(block_time).unwrap();

        let mut block_vrf = [0u8; 32];
        block_vrf[0] = 1;
        block_vrf[31] = block_height as u8;
        let block_vrf = VRFSeed::from_bytes(&block_vrf).unwrap();

        let mut header_hash = vec![0u8; 32];
        header_hash[0] = 2;
        header_hash[31] = block_height as u8;
        let header_hash = BlockHeaderHash::from_bytes(&header_hash).unwrap();

        let mut burnchain_header_hash = vec![0u8; 32];
        burnchain_header_hash[0] = 3;
        burnchain_header_hash[31] = block_height as u8;
        let burnchain_header_hash = BurnchainHeaderHash::from_bytes(&burnchain_header_hash).unwrap();

        self.execute("INSERT INTO simmed_block_table 
                        (block_height, block_time, block_vrf_seed, block_header_hash, burnchain_block_header_hash) 
                        VALUES (?1, ?2, ?3, ?4, ?5)",
                        &[&block_height as &ToSql, &block_time,
                        &block_vrf.to_bytes().to_vec(),
                        &header_hash.to_bytes().to_vec(),
                        &burnchain_header_hash.to_bytes().to_vec()]);
    }

    pub fn sim_mine_block(&mut self) {
        let current_height = self.get_simmed_block_height()
            .expect("Failed to get simulated block height");
        let current_time = self.get_simmed_block_time(current_height)
            .expect("Failed to get simulated block time");

        let block_time = current_time.checked_add(SIMMED_BLOCK_TIME)
            .expect("Integer overflow while increasing simulated block time");
        self.sim_mine_block_with_time(block_time);
    }

    pub fn sim_mine_blocks(&mut self, count: u32) {
        for i in 0..count {
            self.sim_mine_block();
        }
    }
    
    pub fn roll_back(&mut self) {
        self.savepoint.rollback()
            .expect(SQL_FAIL_MESSAGE);
    }

    pub fn commit(self) {
        self.savepoint.commit()
            .expect(SQL_FAIL_MESSAGE);
    }
}

impl <'a> ContractDatabaseTransacter for ContractDatabase<'a> {
    fn begin_save_point(&mut self) -> ContractDatabase {
        let sp = self.savepoint.savepoint()
            .expect(SQL_FAIL_MESSAGE);
        ContractDatabase::from_savepoint(sp)
    }
}
