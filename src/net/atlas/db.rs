use rusqlite::types::ToSql;
use rusqlite::Row;
use rusqlite::Transaction;
use rusqlite::{Connection, OpenFlags, NO_PARAMS};

use std::collections::HashSet;
use std::convert::From;
use std::convert::TryFrom;
use std::fs;

use util::db::tx_begin_immediate;
use util::db::DBConn;
use util::db::Error as db_error;
use util::db::{query_count, query_row, query_rows, query_int, u64_to_sql, FromColumn, FromRow};

use util;
use util::hash::{bin_bytes, hex_bytes, to_bin, to_hex, Hash160};
use util::log;
use util::macros::is_big_endian;
use util::secp256k1::Secp256k1PrivateKey;
use util::secp256k1::Secp256k1PublicKey;

use net::StacksMessageCodec;
use chainstate::stacks::StacksBlockId;
use chainstate::burn::{ConsensusHash, BlockHeaderHash};

use super::inv::{AttachmentInstance};
use super::{Attachment};

pub const ATLASDB_VERSION: &'static str = "23.0.0.0";

const ATLASDB_SETUP: &'static [&'static str] = &[
    r#"
    CREATE TABLE attachments(
        hash TEXT UNIQUE PRIMARY KEY,
        content BLOB NOT NULL
    );"#,
    r#"
    CREATE TABLE attachment_instances(
        id INTEGER PRIMARY KEY AUTOINCREMENT,
        content_hash TEXT,
        created_at INTEGER NOT NULL,
        consensus_hash STRING NOT NULL,
        block_header_hash STRING NOT NULL,
        block_id STRING NOT NULL,
        position_in_page INTEGER NOT NULL,
        page_index INTEGER NOT NULL,
        block_height INTEGER NOT NULL,
        is_available INTEGER NOT NULL,
        metadata STRING NOT NULL
    );"#,
    r#"
    CREATE TABLE inboxed_attachments(
        hash TEXT UNIQUE PRIMARY KEY,
        created_at INTEGER NOT NULL,
        content BLOB NOT NULL
    );"#,
    r#"
    CREATE TABLE db_version(version TEXT NOT NULL);
    "#
];

impl FromRow<Attachment> for Attachment {
    fn from_row<'a>(row: &'a Row) -> Result<Attachment, db_error> {
        let content: Vec<u8> = row.get("content");
        let hex_hash: String = row.get("hash");
        let hash = Hash160::from_hex(&hex_hash)
            .map_err(|_| db_error::TypeError)?;
        Ok(Attachment {
            content,
            hash
        })
    }
}

impl FromRow<AttachmentInstance> for AttachmentInstance {
    fn from_row<'a>(row: &'a Row) -> Result<AttachmentInstance, db_error> {
        let hex_content_hash: String = row.get("content_hash");
        let position_in_page: u32 = row.get("position_in_page");
        let page_index: u32 = row.get("page_index");
        let block_height = u64::from_column(row, "block_height")
            .map_err(|_| db_error::TypeError)?;
        let content_hash = Hash160::from_hex(&hex_content_hash)
            .map_err(|_| db_error::TypeError)?;
        let consensus_hash = ConsensusHash::from_column(row, "consensus_hash")?;
        let block_header_hash = BlockHeaderHash::from_column(row, "block_header_hash")?;
        let metadata: String = row.get("metadata");

        Ok(AttachmentInstance {
            content_hash,
            position_in_page,
            page_index,
            consensus_hash,
            block_header_hash,
            block_height,
            metadata,
        })
    }
}

#[derive(Debug)]
pub struct AtlasDB {
    pub conn: Connection,
    pub readwrite: bool,
}

impl AtlasDB {
    fn instantiate(&mut self) -> Result<(), db_error> {
        let tx = self.tx_begin()?;

        for row_text in ATLASDB_SETUP {
            tx.execute(row_text, NO_PARAMS)
                .map_err(db_error::SqliteError)?;
        }

        tx.execute(
            "INSERT INTO db_version (version) VALUES (?1)",
            &[&ATLASDB_VERSION],
        )
        .map_err(db_error::SqliteError)?;

        tx.commit().map_err(db_error::SqliteError)?;

        Ok(())
    }

    // Open the burn database at the given path.  Open read-only or read/write.
    // If opened for read/write and it doesn't exist, instantiate it.
    pub fn connect(
        path: &String,
        readwrite: bool,
    ) -> Result<AtlasDB, db_error> {
        let mut create_flag = false;
        let open_flags = if fs::metadata(path).is_err() {
            // need to create
            if readwrite {
                create_flag = true;
                OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
            } else {
                return Err(db_error::NoDBError);
            }
        } else {
            // can just open
            if readwrite {
                OpenFlags::SQLITE_OPEN_READ_WRITE
            } else {
                OpenFlags::SQLITE_OPEN_READ_ONLY
            }
        };
        let conn =
            Connection::open_with_flags(path, open_flags).map_err(|e| db_error::SqliteError(e))?;

        let mut db = AtlasDB {
            conn: conn,
            readwrite: readwrite,
        };
        if create_flag {
            db.instantiate()?;
        }
        Ok(db)
    }

    // Open a burn database in memory (used for testing)
    #[cfg(test)]
    pub fn connect_memory() -> Result<AtlasDB, db_error> {
        let conn = Connection::open_in_memory().map_err(|e| db_error::SqliteError(e))?;

        let mut db = AtlasDB {
            conn: conn,
            readwrite: true,
        };

        db.instantiate()?;
        Ok(db)
    }

    pub fn conn(&self) -> &Connection {
        &self.conn
    }

    pub fn tx_begin<'a>(&'a mut self) -> Result<Transaction<'a>, db_error> {
        if !self.readwrite {
            return Err(db_error::ReadOnly);
        }

        let tx = tx_begin_immediate(&mut self.conn)?;
        Ok(tx)
    }

    pub fn get_minmax_heights_window_for_page_index(&self, oldest_page_index: u32, newest_page_index: u32) -> Result<(u64, u64), db_error> {
        let qry = "SELECT MIN(block_height) as min, MAX(block_height) as max FROM attachment_instances WHERE page_index >= ?1 AND page_index <= ?2".to_string();
        let args = [&oldest_page_index as &dyn ToSql, &newest_page_index as &dyn ToSql];
        let mut stmt = self.conn.prepare(&qry)?;
        let mut rows = stmt.query(&args)?;
        match rows.next() {
            Some(Ok(row)) => {
                let min: i64 = row.get_checked("min")?;
                let max: i64 = row.get_checked("max")?;
                Ok((min as u64, max as u64))                
            }
            _ => {
                Err(db_error::NotFoundError)
            }
        }
    }

    pub fn get_attachments_available_at_pages_indexes(&self, pages_indexes: &Vec<u32>, blocks_ids: &Vec<StacksBlockId>) -> Result<Vec<Vec<u8>>, db_error> {
        let mut pages = vec![];
        for page_index in pages_indexes {
            let page = self.get_attachments_missing_at_page_index(*page_index, blocks_ids)?;
            let mut bit_vector = vec![];
            let mut byte: u8 = 0;
            for (index, is_attachment_missing) in page.iter().enumerate() {
                if (index + 1) % 8 == 0 {
                    bit_vector.push(byte);
                    byte = 0;
                }
                byte <<= 1;
                byte ^= if *is_attachment_missing {0} else {1};
            }
            bit_vector.push(byte);
            pages.push(bit_vector);
        }
        Ok(pages)
    }

    pub fn get_attachments_missing_at_page_index(&self, page_index: u32, blocks_ids: &Vec<StacksBlockId>) -> Result<Vec<bool>, db_error> {
        // todo(ludo): unable to build a compiled stmt with rusqlite that includes a WHERE ... IN () clause - investigate carray.
        let ancestor_tree_sql = blocks_ids.iter()
            .map(|block_id| format!("'{}'", block_id))
            .collect::<Vec<String>>()
            .join(", ");
        let qry = format!("SELECT is_available FROM attachment_instances WHERE page_index = {} AND block_id IN ({}) ORDER BY position_in_page ASC", page_index, ancestor_tree_sql);
        let rows = query_rows::<i64, _>(&self.conn, &qry, NO_PARAMS)?;
        let res = rows.iter().map(|r| *r == 0).collect::<Vec<bool>>();
        println!("get_attachments_missing_at_page_index res: {:?}", res);
        Ok(res)
    }

    pub fn insert_new_inboxed_attachment(&mut self, attachment: Attachment) -> Result<(), db_error> {
        // Do we already have an entry (proceessed or unprocessed) for this attachment? - todo(ludo) think more about this
        let qry = "SELECT count(*) FROM inboxed_attachments WHERE hash = ?1".to_string();
        let args = [&attachment.hash as &dyn ToSql];
        let count = query_count(&self.conn, &qry, &args)?;
        if count != 0 {
            return Ok(())
        }

        let tx = self.tx_begin()?;
        let now = util::get_epoch_time_secs() as i64;
        let res = tx.execute(
            "INSERT INTO inboxed_attachments (hash, content, created_at) VALUES (?1, ?2, ?3)",
            &[
                &attachment.hash as &dyn ToSql, 
                &attachment.content as &dyn ToSql,
                &now as &dyn ToSql
            ]
        );
        res.map_err(db_error::SqliteError)?;
        tx.commit().map_err(db_error::SqliteError)?;
        Ok(())
    }

    pub fn find_inboxed_attachment(&mut self, content_hash: &Hash160) -> Result<Option<Attachment>, db_error> {
        let hex_content_hash = to_hex(&content_hash.0[..]);
        let qry = "SELECT content, hash FROM inboxed_attachments WHERE hash = ?1".to_string();
        let args = [&hex_content_hash as &dyn ToSql];
        let row = query_row::<Attachment, _>(&self.conn, &qry, &args)?;
        Ok(row)
    }

    pub fn find_all_attachment_instances(&mut self, content_hash: &Hash160) -> Result<Vec<AttachmentInstance>, db_error> {
        let hex_content_hash = to_hex(&content_hash.0[..]);
        let qry = "SELECT * FROM attachment_instances WHERE content_hash = ?1".to_string();
        let args = [&hex_content_hash as &dyn ToSql];
        let rows = query_rows::<AttachmentInstance, _>(&self.conn, &qry, &args)?;
        Ok(rows)
    }

    pub fn find_attachment(&mut self, content_hash: &Hash160) -> Result<Option<Attachment>, db_error> {
        let hex_content_hash = to_hex(&content_hash.0[..]);
        let qry = "SELECT content, hash FROM attachments WHERE hash = ?1".to_string();
        let args = [&hex_content_hash as &dyn ToSql];
        let row = query_row::<Attachment, _>(&self.conn, &qry, &args)?;
        Ok(row)
    }

    pub fn insert_new_attachment_instance(&mut self, attachment: &AttachmentInstance, is_available: bool) -> Result<(), db_error> {
        let hex_content_hash = to_hex(&attachment.content_hash.0[..]);
        let tx = self.tx_begin()?;
        let now = util::get_epoch_time_secs() as i64;
        let res = tx.execute(
            "INSERT INTO attachment_instances (content_hash, created_at, block_id, position_in_page, page_index, block_height, is_available, metadata, consensus_hash, block_header_hash) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            &[
                &hex_content_hash as &dyn ToSql, 
                &now as &dyn ToSql,
                &attachment.get_stacks_block_id() as &dyn ToSql, 
                &attachment.position_in_page as &dyn ToSql, 
                &attachment.page_index as &dyn ToSql, 
                &u64_to_sql(attachment.block_height)?,
                &is_available as &dyn ToSql,
                &attachment.metadata as &dyn ToSql,
                &attachment.consensus_hash as &dyn ToSql,
                &attachment.block_header_hash as &dyn ToSql,
            ]
        );
        res.map_err(db_error::SqliteError)?;
        tx.commit().map_err(db_error::SqliteError)?;
        Ok(())
    }

    pub fn insert_new_attachment(&mut self, content_hash: &Hash160, content: &Vec<u8>, cascade_update: bool) -> Result<(), db_error> {
        let hex_content_hash = to_hex(&content_hash.0[..]);
        let tx = self.tx_begin()?;
        tx.execute(
            "INSERT INTO attachments (hash, content) VALUES (?1, ?2)",
            &[
                &hex_content_hash as &dyn ToSql, 
                &content as &dyn ToSql,
            ]
        ).map_err(db_error::SqliteError)?;
        if cascade_update {
            tx.execute(
                "UPDATE attachment_instances SET is_available = 1 WHERE content_hash = ?1",
                &[&hex_content_hash as &dyn ToSql],
            ).map_err(db_error::SqliteError)?;    
        }
        tx.commit().map_err(db_error::SqliteError)?;
        Ok(())
    }

    pub fn remove_attachment_from_inbox(&mut self, content_hash: &Hash160) -> Result<(), db_error> {
        let hex_content_hash = to_hex(&content_hash.0[..]);
        let tx = self.tx_begin()?;
        tx.execute(
            "DELETE FROM inboxed_attachments WHERE hash = ?1",
            &[&hex_content_hash as &dyn ToSql,],
        ).map_err(db_error::SqliteError)?;
        tx.commit().map_err(db_error::SqliteError)?;
        Ok(())
    }

    pub fn import_attachment_from_inbox(&mut self, content_hash: &Hash160) -> Result<(), db_error> {

        let attachment = match self.find_inboxed_attachment(content_hash) {
            Ok(Some(attachment)) => attachment,
            _ => return Err(db_error::NotFoundError)
        };

        self.insert_new_attachment(&attachment.hash, &attachment.content, true)?;
        self.remove_attachment_from_inbox(&attachment.hash)?;
        Ok(())
    }
}
